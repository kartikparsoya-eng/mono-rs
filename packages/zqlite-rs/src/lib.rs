#![deny(clippy::all)]

pub mod advance;
pub mod connection_pool;
pub mod database;
pub mod diff;
pub mod row_iterator;
pub mod statement;
pub mod source;
pub mod query_builder;
mod types;

// Re-exports happen automatically via #[napi] attribute.
// napi-rs generates index.js and index.d.ts with all exported classes/functions.
pub mod overlay;
pub mod table_source;
pub mod hydrate;
pub mod ast_to_config;
pub mod pipeline_manager;
pub mod chunk_encoder;
