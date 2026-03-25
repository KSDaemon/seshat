// Re-export public API — barrel-style exports for Rust
pub mod error;
pub mod handlers;
pub mod middleware;
pub mod models;
pub mod services;
pub mod utils;

pub use models::{AppState, User, UserId};
pub use services::user_service;
