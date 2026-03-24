//! # Seshat
//!
//! Binary entry point for the Seshat CLI tool and MCP server.
//! Wires all crates together: config loading, runtime initialization,
//! command dispatch.

fn main() {
    println!("seshat {}", env!("CARGO_PKG_VERSION"));
}
