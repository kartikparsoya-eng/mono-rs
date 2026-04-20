#![deny(clippy::all)]

use napi_derive::napi;

/// Smoke-test export to verify napi-rs toolchain works
#[napi]
pub fn hello() -> String {
    "zqlite-rs loaded".to_string()
}
