use std::collections::HashMap;
use std::sync::Arc;

use todo_core::{Config, DbError, Row, Runtime, Stmt, Value};
use tower_service::Service;
use worker::send::SendFuture;
use worker::wasm_bindgen::JsValue;
use worker::{
    Context, D1Database, D1PreparedStatement, Env, HttpRequest, console_error, event, js_sys,
};

#[derive(Clone)]
struct D1Runtime {
    db: Arc<D1Database>,
}

fn db_error(e: worker::Error) -> DbError {
    let message = e.to_string();
    if message.contains("UNIQUE constraint failed") {
        DbError::UniqueViolation
    } else {
        DbError::Other(message)
    }
}

/// D1 cannot bind 64-bit integers; millisecond timestamps fit in an f64 exactly.
fn to_js(params: &[Value]) -> Vec<JsValue> {
    params
        .iter()
        .map(|p| match p {
            Value::Null => JsValue::NULL,
            Value::Int(i) => JsValue::from_f64(*i as f64),
            Value::Text(s) => JsValue::from_str(s),
        })
        .collect()
}

fn prepare(db: &D1Database, sql: &str, params: &[Value]) -> Result<D1PreparedStatement, DbError> {
    db.prepare(sql).bind(&to_js(params)).map_err(db_error)
}

fn convert_row(object: serde_json::Map<String, serde_json::Value>) -> Row {
    let columns: HashMap<String, Value> = object
        .into_iter()
        .map(|(name, value)| {
            let value = match value {
                serde_json::Value::Null => Value::Null,
                serde_json::Value::Number(n) => Value::Int(
                    n.as_i64()
                        .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
                ),
                serde_json::Value::String(s) => Value::Text(s),
                other => Value::Text(other.to_string()),
            };
            (name, value)
        })
        .collect();
    Row::new(columns)
}

impl Runtime for D1Runtime {
    fn query(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> impl Future<Output = Result<Vec<Row>, DbError>> + Send {
        let db = self.db.clone();
        let sql = sql.to_owned();
        SendFuture::new(async move {
            let result = prepare(&db, &sql, &params)?.all().await.map_err(db_error)?;
            let rows: Vec<serde_json::Map<String, serde_json::Value>> =
                result.results().map_err(db_error)?;
            Ok(rows.into_iter().map(convert_row).collect())
        })
    }

    fn execute(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> impl Future<Output = Result<u64, DbError>> + Send {
        let db = self.db.clone();
        let sql = sql.to_owned();
        SendFuture::new(async move {
            let result = prepare(&db, &sql, &params)?.run().await.map_err(db_error)?;
            let changes = result
                .meta()
                .map_err(db_error)?
                .and_then(|m| m.changes)
                .unwrap_or(0);
            Ok(changes as u64)
        })
    }

    fn batch(&self, stmts: Vec<Stmt>) -> impl Future<Output = Result<(), DbError>> + Send {
        let db = self.db.clone();
        SendFuture::new(async move {
            let prepared = stmts
                .iter()
                .map(|s| prepare(&db, &s.sql, &s.params))
                .collect::<Result<Vec<_>, _>>()?;
            db.batch(prepared).await.map_err(db_error)?;
            Ok(())
        })
    }

    fn now_ms(&self) -> i64 {
        js_sys::Date::now() as i64
    }

    fn fill_random(&self, buf: &mut [u8]) {
        let array = js_sys::Uint8Array::new_with_length(buf.len() as u32);
        let crypto = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))
            .expect("Workers always provide crypto");
        let get_random_values: js_sys::Function =
            js_sys::Reflect::get(&crypto, &JsValue::from_str("getRandomValues"))
                .expect("crypto.getRandomValues exists")
                .into();
        get_random_values
            .call1(&crypto, &array)
            .expect("crypto.getRandomValues failed");
        array.copy_to(buf);
    }

    fn log_error(&self, message: &str) {
        console_error!("{message}");
    }
}

/// Plain `[vars]` first, then secrets (`wrangler secret put SETUP_TOKEN`).
fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name)
        .or_else(|_| env.secret(name))
        .ok()
        .map(|v| v.to_string())
}

#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> worker::Result<axum::http::Response<axum::body::Body>> {
    let runtime = D1Runtime {
        db: Arc::new(env.d1("DB")?),
    };
    let config = Config::from_vars(|name| var(&env, name));
    Ok(todo_core::router(runtime, config).call(req).await?)
}
