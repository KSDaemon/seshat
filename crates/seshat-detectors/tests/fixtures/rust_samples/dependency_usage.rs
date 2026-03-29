// Sample: Dependency usage patterns for Rust
// Expected detections: canonical HTTP library (reqwest), canonical logging library (tracing),
// canonical serialization library (serde), canonical async runtime (tokio),
// conflicting logging libraries (tracing + log)

use reqwest::Client;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use log::debug;
use tokio::runtime::Runtime;
use clap::Parser;

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Value,
}

pub async fn fetch_data(client: &Client, url: &str) -> Result<ApiResponse, reqwest::Error> {
    info!(url, "Fetching data");
    let resp = client.get(url).send().await?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await?;
    debug!("Response received: {}", status);
    Ok(ApiResponse { status, body })
}

pub fn build_headers() -> HeaderMap {
    warn!("Using default headers");
    HeaderMap::new()
}

#[derive(Parser)]
pub struct Cli {
    #[arg(short, long)]
    pub url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_response_serialize() {
        let resp = ApiResponse {
            status: 200,
            body: Value::Null,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("200"));
    }
}
