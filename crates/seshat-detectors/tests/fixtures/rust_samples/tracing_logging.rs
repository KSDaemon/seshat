// Sample: tracing-based structured logging patterns
// Expected detections: tracing dependency, info!/warn!/error! macros, #[instrument]

use tracing::{error, info, instrument, warn};

pub struct RequestContext {
    pub request_id: String,
    pub user_agent: String,
}

#[instrument(skip(ctx), fields(request_id = %ctx.request_id))]
pub fn process_request(ctx: &RequestContext, path: &str) -> Result<String, String> {
    info!(path, "Processing incoming request");

    if path.starts_with("/admin") {
        warn!(path, "Admin endpoint accessed");
    }

    if path.is_empty() {
        error!("Empty path received");
        return Err("Empty path".into());
    }

    info!("Request processed successfully");
    Ok(format!("Response for {path}"))
}

#[instrument]
pub fn health_check() -> &'static str {
    info!("Health check called");
    "ok"
}

fn log_metrics(count: u64, duration_ms: u64) {
    info!(count, duration_ms, "Batch processing complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_request() {
        let ctx = RequestContext {
            request_id: "req-1".into(),
            user_agent: "test".into(),
        };
        let result = process_request(&ctx, "/api/users");
        assert!(result.is_ok());
    }

    #[test]
    fn test_empty_path() {
        let ctx = RequestContext {
            request_id: "req-2".into(),
            user_agent: "test".into(),
        };
        let result = process_request(&ctx, "");
        assert!(result.is_err());
    }

    #[test]
    fn test_health_check() {
        assert_eq!(health_check(), "ok");
    }
}
