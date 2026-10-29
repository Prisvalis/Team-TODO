//! Port to the runtime: database access, clock, randomness and logging.
//!
//! Workers have no `SystemTime` and need special setup for `getrandom`, so the
//! clock and randomness come from the adapter alongside the database.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Text(String),
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Int(i64::from(v))
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

/// One result row keyed by column name (use `AS` aliases for joined columns).
#[derive(Debug, Clone, Default)]
pub struct Row(HashMap<String, Value>);

impl Row {
    pub fn new(columns: HashMap<String, Value>) -> Self {
        Row(columns)
    }

    fn get(&self, column: &str) -> &Value {
        self.0.get(column).unwrap_or(&Value::Null)
    }

    pub fn text(&self, column: &str) -> String {
        self.opt_text(column).unwrap_or_default()
    }

    pub fn opt_text(&self, column: &str) -> Option<String> {
        match self.get(column) {
            Value::Text(s) => Some(s.clone()),
            Value::Int(i) => Some(i.to_string()),
            Value::Null => None,
        }
    }

    pub fn int(&self, column: &str) -> i64 {
        self.opt_int(column).unwrap_or_default()
    }

    pub fn opt_int(&self, column: &str) -> Option<i64> {
        match self.get(column) {
            Value::Int(i) => Some(*i),
            Value::Text(s) => s.parse().ok(),
            Value::Null => None,
        }
    }

    pub fn flag(&self, column: &str) -> bool {
        self.int(column) != 0
    }
}

#[derive(Debug)]
pub enum DbError {
    /// A UNIQUE or PRIMARY KEY constraint rejected the write.
    UniqueViolation,
    Other(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::UniqueViolation => f.write_str("unique constraint violation"),
            DbError::Other(msg) => write!(f, "database error: {msg}"),
        }
    }
}

impl std::error::Error for DbError {}

pub struct Stmt {
    pub sql: String,
    pub params: Vec<Value>,
}

impl Stmt {
    pub fn new(sql: impl Into<String>, params: Vec<Value>) -> Self {
        Stmt {
            sql: sql.into(),
            params,
        }
    }
}

/// Implemented once per deployment target (MariaDB via sqlx, D1 via workers-rs).
///
/// Futures must be `Send` because axum requires it; the Worker side satisfies this
/// with `SendFuture` since wasm is single-threaded.
pub trait Runtime: Clone + Send + Sync + 'static {
    fn query(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> impl Future<Output = Result<Vec<Row>, DbError>> + Send;

    /// Returns the number of affected rows.
    fn execute(
        &self,
        sql: &str,
        params: Vec<Value>,
    ) -> impl Future<Output = Result<u64, DbError>> + Send;

    /// Runs all statements atomically (D1 has no interactive transactions).
    fn batch(&self, stmts: Vec<Stmt>) -> impl Future<Output = Result<(), DbError>> + Send;

    /// Current UTC time in Unix milliseconds.
    fn now_ms(&self) -> i64;

    fn fill_random(&self, buf: &mut [u8]);

    fn log_error(&self, message: &str);
}
