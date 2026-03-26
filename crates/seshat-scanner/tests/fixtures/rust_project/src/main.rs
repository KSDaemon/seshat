// Fixture: A realistic Rust main.rs for integration testing

use std::io::{self, Read, Write};
use serde::{Deserialize, Serialize};

mod config;
mod error;
mod server;

/// Application entry point.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = config::Config::load()?;
    println!("Starting with config: {:?}", config);
    Ok(())
}

fn main() {
    println!("Hello, world!");
}
