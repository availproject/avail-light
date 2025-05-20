pub mod configuration;
#[cfg(not(target_arch = "wasm32"))]
pub mod diagnostics;
#[cfg(not(target_arch = "wasm32"))]
pub mod routes;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
pub mod types;
#[cfg(not(target_arch = "wasm32"))]
mod v1;
pub mod v2;
