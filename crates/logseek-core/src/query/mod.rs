//! The query language: parsing, execution and aggregation over an index.

pub mod agg;
pub mod ast;
pub mod contrast;
pub mod exec;
pub mod follow;
pub mod group;
pub mod links;
pub mod multi;
pub mod newin;
pub mod order;
pub mod pairs;
mod parser;
pub mod patterns;
pub mod plain;
mod render;
pub mod rhythm;
pub mod roles;
pub mod search;
pub mod shape;
pub mod sort;
pub mod strange;
pub mod unusual;

pub use parser::{parse, parse_span, ParseError};
pub use render::{field_text, to_string, value_text};
