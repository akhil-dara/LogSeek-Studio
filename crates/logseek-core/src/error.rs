//! The error type shared across the engine.

use crate::budget::BudgetExceeded;
use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Budget(BudgetExceeded),
    Corrupt(String),
    Cancelled,
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Budget(e) => write!(f, "{e}"),
            Error::Corrupt(s) => write!(f, "corrupt data: {s}"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::Invalid(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<BudgetExceeded> for Error {
    fn from(e: BudgetExceeded) -> Self {
        Error::Budget(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Corrupt(e.to_string())
    }
}
