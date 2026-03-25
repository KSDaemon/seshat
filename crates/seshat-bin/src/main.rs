//! # Seshat
//!
//! Binary entry point for the Seshat CLI tool and MCP server.
//! Wires all crates together: config loading, runtime initialization,
//! command dispatch.

pub mod config;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!(
            "seshat {} ({})",
            env!("CARGO_PKG_VERSION"),
            env!("GIT_HASH")
        );
        return;
    }

    println!(
        "seshat {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("GIT_HASH")
    );
}
