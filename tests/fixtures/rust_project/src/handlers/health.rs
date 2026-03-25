// Demonstrates:
// - Simple public function
// - serde Serialize derive
// - JSON response pattern

use serde::Serialize;

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
}

/// Returns a health check response.
pub fn health_handler() -> HealthResponse {
    HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_response() {
        let resp = health_handler();
        assert_eq!(resp.status, "ok");
    }
}
