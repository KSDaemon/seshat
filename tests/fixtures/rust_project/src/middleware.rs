// Demonstrates:
// - Trait definitions
// - Async trait pattern (manual, no async-trait crate)
// - Tracing spans and events
// - pub/private functions
// - Generic constraints

use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use tracing::{info_span, Instrument};

/// Middleware trait for request processing.
pub trait Middleware: Send + Sync {
    fn name(&self) -> &str;
    fn process<'a>(
        &'a self,
        request: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

/// Logging middleware that records request timing.
pub struct LoggingMiddleware;

impl Middleware for LoggingMiddleware {
    fn name(&self) -> &str {
        "logging"
    }

    fn process<'a>(
        &'a self,
        request: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
        Box::pin(async move {
            let start = Instant::now();
            let span = info_span!("request", path = %request);

            async move {
                tracing::info!("Processing request");
                let elapsed = start.elapsed();
                tracing::info!(duration_ms = elapsed.as_millis(), "Request completed");
                Ok(format!("processed: {request}"))
            }
            .instrument(span)
            .await
        })
    }
}

/// Runs a request through a chain of middleware.
pub async fn run_middleware_chain(
    middlewares: &[Box<dyn Middleware>],
    request: &str,
) -> Result<String, String> {
    let mut result = request.to_owned();
    for mw in middlewares {
        result = mw.process(&result).await?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_logging_middleware() {
        let mw = LoggingMiddleware;
        let result = mw.process("/health").await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("/health"));
    }

    #[tokio::test]
    async fn test_middleware_chain() {
        let chain: Vec<Box<dyn Middleware>> = vec![Box::new(LoggingMiddleware)];
        let result = run_middleware_chain(&chain, "/api/users").await;
        assert!(result.is_ok());
    }
}
