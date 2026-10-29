use std::collections::HashMap;
use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::mysql::{MySqlArguments, MySqlConnectOptions, MySqlPoolOptions, MySqlRow};
use sqlx::query::Query;
use sqlx::{AssertSqlSafe, Column, MySql, MySqlPool, Row as _, TypeInfo, ValueRef};
use todo_core::{Config, DbError, Row, Runtime, Stmt, Value};

#[derive(Clone)]
struct MySqlRuntime {
    pool: MySqlPool,
}

/// todo-core builds SQL only from its own constants and binds every value as a
/// parameter, so the dynamic SQL string is safe to pass through.
fn prepared(sql: &str, params: Vec<Value>) -> Query<'static, MySql, MySqlArguments> {
    let mut query = sqlx::query(AssertSqlSafe(sql.to_owned()));
    for param in params {
        query = match param {
            Value::Null => query.bind(None::<String>),
            Value::Int(i) => query.bind(i),
            Value::Text(s) => query.bind(s),
        };
    }
    query
}

fn db_error(e: sqlx::Error) -> DbError {
    match e.as_database_error() {
        Some(db) if db.is_unique_violation() => DbError::UniqueViolation,
        _ => DbError::Other(e.to_string()),
    }
}

fn convert_row(row: &MySqlRow) -> Result<Row, sqlx::Error> {
    let mut columns = HashMap::new();
    for (i, column) in row.columns().iter().enumerate() {
        let value = if row.try_get_raw(i)?.is_null() {
            Value::Null
        } else if column.type_info().name().contains("INT") {
            Value::Int(row.try_get(i)?)
        } else {
            Value::Text(row.try_get(i)?)
        };
        columns.insert(column.name().to_owned(), value);
    }
    Ok(Row::new(columns))
}

fn is_deadlock(e: &sqlx::Error) -> bool {
    // 1213 = ER_LOCK_DEADLOCK
    e.as_database_error()
        .and_then(|db| db.code())
        .is_some_and(|code| code == "40001" || code == "1213")
}

impl MySqlRuntime {
    async fn run_transaction(&self, stmts: &[Stmt]) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        for stmt in stmts {
            prepared(&stmt.sql, stmt.params.clone())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }
}

impl Runtime for MySqlRuntime {
    async fn query(&self, sql: &str, params: Vec<Value>) -> Result<Vec<Row>, DbError> {
        let rows = prepared(sql, params)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?;
        rows.iter()
            .map(convert_row)
            .collect::<Result<_, _>>()
            .map_err(db_error)
    }

    async fn execute(&self, sql: &str, params: Vec<Value>) -> Result<u64, DbError> {
        prepared(sql, params)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .map_err(db_error)
    }

    async fn batch(&self, stmts: Vec<Stmt>) -> Result<(), DbError> {
        // InnoDB may pick this transaction as a deadlock victim when identical requests
        // race (e.g. a double submit); D1 serialises batches, so retry to match it.
        let mut attempt = 0;
        loop {
            match self.run_transaction(&stmts).await {
                Err(e) if attempt < 2 && is_deadlock(&e) => attempt += 1,
                result => return result.map_err(db_error),
            }
        }
    }

    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    }

    fn fill_random(&self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("OS random source unavailable");
    }

    fn log_error(&self, message: &str) {
        eprintln!("ERROR {message}");
    }
}

fn var(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Docker stops containers with SIGTERM; PID 1 ignores it unless handled.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        if let Ok(mut sigterm) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sigterm.recv().await;
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

/// Comparisons must be byte-exact like SQLite/D1; compose sets this, an external server may not.
async fn warn_on_collation(pool: &MySqlPool) {
    let collation: Result<String, _> = sqlx::query_scalar("SELECT @@collation_database")
        .fetch_one(pool)
        .await;
    if let Ok(collation) = collation
        && collation != "utf8mb4_nopad_bin"
    {
        eprintln!(
            "WARNING database collation is {collation}; use utf8mb4_nopad_bin so usernames, \
             labels and search behave the same as on Cloudflare D1"
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Separate variables instead of a URL: passwords need no percent-encoding.
    let options = MySqlConnectOptions::new()
        .host(&var("DB_HOST", "127.0.0.1"))
        .port(var("DB_PORT", "3306").parse()?)
        .username(&var("DB_USER", "todo"))
        .password(&env::var("DB_PASSWORD").map_err(|_| "DB_PASSWORD must be set")?)
        .database(&var("DB_NAME", "todo"))
        // Shared migrations use `||` for concatenation, as SQLite does.
        .pipes_as_concat(true);
    let pool = MySqlPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await?;
    sqlx::migrate!("../migrations").run(&pool).await?;
    warn_on_collation(&pool).await;

    let config = Config::from_vars(|name| env::var(name).ok());
    let bind_addr = var("BIND_ADDR", "0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!("listening on {bind_addr}");
    axum::serve(
        listener,
        todo_core::router(MySqlRuntime { pool: pool.clone() }, config),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    pool.close().await;
    println!("shut down cleanly");
    Ok(())
}
