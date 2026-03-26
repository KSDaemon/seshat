// Fixture: Rust server module with trait + impl

use std::io::*;

/// A trait for handling requests.
pub trait Handler {
    fn handle(&self, request: &str) -> String;
}

/// A simple echo server.
#[derive(Debug, Clone)]
pub struct EchoServer {
    prefix: String,
}

impl EchoServer {
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_string(),
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn log(&self, msg: &str) {
        println!("[{}] {}", self.prefix, msg);
    }
}

impl Handler for EchoServer {
    fn handle(&self, request: &str) -> String {
        format!("{}: {}", self.prefix, request)
    }
}
