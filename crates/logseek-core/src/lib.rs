//! The LogSeek engine: ingest, the on-disk index, queries and the memory budget.

pub mod budget;
pub mod error;
pub mod extract;
pub mod formula;
pub mod ingest;
pub mod jsonpath;
pub mod query;
pub mod schema;
pub mod segment;
pub mod store;
pub mod sysmem;
pub mod time;
pub mod types;

pub use error::{Error, Result};
