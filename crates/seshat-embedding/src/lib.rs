//! # seshat-embedding
//!
//! Embedding provider abstraction with Ollama and OpenAI implementations.
//!
//! When the `[embedding]` section is present in `seshat.toml`, callers can
//! construct a provider via [`create_provider`] and use it for vector search.
//! When the section is absent, no provider is created and there is zero
//! overhead — no HTTP connections, no allocations, no background work.
//!
//! ## Supported providers
//!
//! | Provider | Default model      | Endpoint                                     |
//! |----------|--------------------|----------------------------------------------|
//! | `ollama` | `all-minilm`       | `http://localhost:11434/api/embed`            |
//! | `openai` | `text-embedding-3-small` | `https://api.openai.com/v1/embeddings`  |

use std::fmt;

use serde::{Deserialize, Serialize};

// ─── Error types ─────────────────────────────────────────────────────────────

/// Errors from embedding operations.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    /// HTTP request failed (timeout, connection refused, etc.).
    #[error("embedding HTTP request failed: {0}")]
    HttpError(String),

    /// Provider returned a non-success status code.
    #[error("embedding provider returned status {status}: {body}")]
    StatusError { status: u16, body: String },

    /// Failed to parse the provider's JSON response.
    #[error("failed to parse embedding response: {0}")]
    ParseError(String),

    /// The provider returned an unexpected number of embedding vectors.
    #[error("expected {expected} embedding vectors, got {got}")]
    CountMismatch { expected: usize, got: usize },

    /// An embedding vector has an unexpected number of dimensions.
    #[error("expected {expected}-dimensional embedding, got {got} dimensions")]
    DimensionMismatch { expected: usize, got: usize },

    /// Configuration error (e.g., missing API key).
    #[error("embedding configuration error: {0}")]
    ConfigError(String),
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Abstraction over embedding providers.
///
/// Implementations must be `Send + Sync` so providers can be shared across
/// threads (e.g., stored in an `Arc`).
pub trait EmbeddingProvider: Send + Sync + fmt::Debug {
    /// Generate embeddings for one or more text inputs.
    ///
    /// Returns one `Vec<f32>` per input text, each of length [`Self::dimension`].
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// The dimensionality of the embedding vectors this provider produces.
    fn dimension(&self) -> usize;
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for the embedding provider, parsed from `[embedding]` in
/// `seshat.toml`.
///
/// When this section is absent, embedding is disabled with zero overhead.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct EmbeddingConfig {
    /// Provider name: `"ollama"` or `"openai"`.
    pub provider: String,
    /// Model name (provider-specific).
    pub model: String,
    /// Embedding vector dimension. When `0`, uses the provider's default.
    pub dimension: usize,
    /// Batch size for embedding generation.
    pub batch_size: usize,
    /// API key for providers that require one (e.g. OpenAI).
    ///
    /// When empty, the provider falls back to reading the corresponding
    /// environment variable (e.g. `OPENAI_API_KEY`).
    /// API key is never serialized to prevent accidental secret leakage
    /// (e.g., in logs, debug output, or config round-trips).
    #[serde(default, skip_serializing)]
    pub api_key: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".to_owned(),
            model: String::new(), // empty → provider default
            dimension: 0,         // 0 → provider default
            batch_size: 32,
            api_key: String::new(),
        }
    }
}

impl fmt::Display for EmbeddingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "provider={}, model={}, dimension={}, batch_size={}",
            self.provider,
            if self.model.is_empty() {
                "(default)"
            } else {
                &self.model
            },
            if self.dimension == 0 {
                "(default)".to_owned()
            } else {
                self.dimension.to_string()
            },
            self.batch_size,
        )
    }
}

// ─── Provider factory ────────────────────────────────────────────────────────

/// Create an embedding provider from configuration.
///
/// # Errors
///
/// Returns [`EmbeddingError::ConfigError`] if the provider name is unknown
/// or required configuration is missing (e.g., `OPENAI_API_KEY` for OpenAI).
pub fn create_provider(
    config: &EmbeddingConfig,
) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
    // Validate batch_size early — 0 would panic in `slice::chunks()`.
    if config.batch_size == 0 {
        return Err(EmbeddingError::ConfigError(
            "batch_size must be at least 1".to_owned(),
        ));
    }

    // Case-insensitive and trimmed provider name matching.
    let provider = config.provider.trim().to_lowercase();

    match provider.as_str() {
        "ollama" => {
            let model = if config.model.is_empty() {
                "all-minilm".to_owned()
            } else {
                config.model.clone()
            };
            let dimension = if config.dimension == 0 {
                384
            } else {
                config.dimension
            };
            Ok(Box::new(OllamaProvider::new(model, dimension)))
        }
        "openai" => {
            // Resolve API key: config field → env var, rejecting whitespace-only values.
            let api_key = if !config.api_key.trim().is_empty() {
                config.api_key.trim().to_owned()
            } else {
                let key = std::env::var("OPENAI_API_KEY").map_err(|_| {
                    EmbeddingError::ConfigError(
                        "OPENAI_API_KEY environment variable is required for the openai provider \
                         (or set api_key in [embedding] config)"
                            .to_owned(),
                    )
                })?;
                let key = key.trim().to_owned();
                if key.is_empty() {
                    return Err(EmbeddingError::ConfigError(
                        "OPENAI_API_KEY is set but empty (whitespace-only)".to_owned(),
                    ));
                }
                key
            };
            let model = if config.model.is_empty() {
                "text-embedding-3-small".to_owned()
            } else {
                config.model.clone()
            };
            let dimension = if config.dimension == 0 {
                1536
            } else {
                config.dimension
            };
            Ok(Box::new(OpenAIProvider::new(api_key, model, dimension)))
        }
        _ => Err(EmbeddingError::ConfigError(format!(
            "unknown embedding provider '{}'. Supported providers: ollama, openai",
            config.provider
        ))),
    }
}

// ─── Agent helper ────────────────────────────────────────────────────────────

/// Create a ureq agent with reasonable defaults for embedding API calls.
fn make_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build();
    config.into()
}

// ─── Ollama provider ─────────────────────────────────────────────────────────

/// Embedding provider using a local Ollama instance.
///
/// Sends POST requests to `http://localhost:11434/api/embed`.
#[derive(Debug)]
pub struct OllamaProvider {
    model: String,
    dimension: usize,
    agent: ureq::Agent,
    endpoint: String,
}

impl OllamaProvider {
    /// Create a new Ollama provider with the given model name and dimension.
    pub fn new(model: String, dimension: usize) -> Self {
        Self {
            model,
            dimension,
            agent: make_agent(),
            endpoint: "http://localhost:11434/api/embed".to_owned(),
        }
    }

    /// Create a new Ollama provider with a custom endpoint (for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    fn with_endpoint(model: String, dimension: usize, endpoint: String) -> Self {
        Self {
            model,
            dimension,
            agent: make_agent(),
            endpoint,
        }
    }
}

impl EmbeddingProvider for OllamaProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Ollama /api/embed accepts {"model": "...", "input": ["text1", "text2", ...]}
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let mut response = self
            .agent
            .post(&self.endpoint)
            .send_json(&body)
            .map_err(map_ureq_error)?;

        let json: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|e| EmbeddingError::ParseError(e.to_string()))?;

        // Response: {"embeddings": [[0.1, 0.2, ...], [0.3, 0.4, ...]]}
        parse_ollama_response(&json, texts.len())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Parse Ollama embedding response JSON.
fn parse_ollama_response(
    json: &serde_json::Value,
    expected_count: usize,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    // Check for provider-level error (e.g., "model not found") before parsing embeddings.
    if let Some(error) = json.get("error").and_then(serde_json::Value::as_str) {
        return Err(EmbeddingError::ParseError(format!(
            "Ollama returned error: {error}"
        )));
    }

    let embeddings = json
        .get("embeddings")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            EmbeddingError::ParseError("missing 'embeddings' array in response".to_owned())
        })?;

    if embeddings.len() != expected_count {
        return Err(EmbeddingError::CountMismatch {
            expected: expected_count,
            got: embeddings.len(),
        });
    }

    let vecs: Vec<Vec<f32>> = embeddings
        .iter()
        .map(parse_f32_array)
        .collect::<Result<_, _>>()?;

    // Validate: no empty vectors (would cause division-by-zero in cosine similarity).
    for (i, v) in vecs.iter().enumerate() {
        if v.is_empty() {
            return Err(EmbeddingError::ParseError(format!(
                "embedding at index {i} is empty"
            )));
        }
    }

    Ok(vecs)
}

// ─── OpenAI provider ─────────────────────────────────────────────────────────

/// Embedding provider using the OpenAI API.
///
/// Sends POST requests to `https://api.openai.com/v1/embeddings`.
/// Requires the `OPENAI_API_KEY` environment variable.
pub struct OpenAIProvider {
    api_key: String,
    model: String,
    dimension: usize,
    agent: ureq::Agent,
    endpoint: String,
}

impl OpenAIProvider {
    /// Create a new OpenAI provider.
    pub fn new(api_key: String, model: String, dimension: usize) -> Self {
        Self {
            api_key,
            model,
            dimension,
            agent: make_agent(),
            endpoint: "https://api.openai.com/v1/embeddings".to_owned(),
        }
    }

    /// Create a new OpenAI provider with a custom endpoint (for testing).
    #[cfg(test)]
    #[allow(dead_code)]
    fn with_endpoint(api_key: String, model: String, dimension: usize, endpoint: String) -> Self {
        Self {
            api_key,
            model,
            dimension,
            agent: make_agent(),
            endpoint,
        }
    }
}

impl fmt::Debug for OpenAIProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAIProvider")
            .field("model", &self.model)
            .field("dimension", &self.dimension)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive() // hide api_key
    }
}

impl EmbeddingProvider for OpenAIProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // OpenAI /v1/embeddings: {"input": [...], "model": "..."}
        let body = serde_json::json!({
            "input": texts,
            "model": self.model,
        });

        let mut response = self
            .agent
            .post(&self.endpoint)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send_json(&body)
            .map_err(map_ureq_error)?;

        let json: serde_json::Value = response
            .body_mut()
            .read_json()
            .map_err(|e| EmbeddingError::ParseError(e.to_string()))?;

        // Response: {"data": [{"embedding": [0.1, ...], "index": 0}, ...]}
        parse_openai_response(&json, texts.len())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Parse OpenAI embedding response JSON.
fn parse_openai_response(
    json: &serde_json::Value,
    expected_count: usize,
) -> Result<Vec<Vec<f32>>, EmbeddingError> {
    // Check for API-level error before parsing data.
    if let Some(error) = json.get("error") {
        let msg = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown error");
        return Err(EmbeddingError::ParseError(format!(
            "OpenAI returned error: {msg}"
        )));
    }

    let data = json
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| EmbeddingError::ParseError("missing 'data' array in response".to_owned()))?;

    if data.len() != expected_count {
        return Err(EmbeddingError::CountMismatch {
            expected: expected_count,
            got: data.len(),
        });
    }

    // OpenAI returns items sorted by index, but sort explicitly to be safe.
    let mut items: Vec<(usize, Vec<f32>)> = data
        .iter()
        .enumerate()
        .map(|(pos, item)| {
            let index = item
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| {
                    EmbeddingError::ParseError(format!(
                        "missing 'index' field in data item at position {pos}"
                    ))
                })? as usize;
            let embedding = item
                .get("embedding")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    EmbeddingError::ParseError("missing 'embedding' in data item".to_owned())
                })?;
            let vec = embedding
                .iter()
                .map(|v| {
                    let f64_val = v.as_f64().ok_or_else(|| {
                        EmbeddingError::ParseError("embedding value is not a number".to_owned())
                    })?;
                    let f32_val = f64_val as f32;
                    if !f32_val.is_finite() {
                        return Err(EmbeddingError::ParseError(format!(
                            "embedding value is not finite: {f64_val}"
                        )));
                    }
                    Ok(f32_val)
                })
                .collect::<Result<Vec<f32>, _>>()?;
            Ok((index, vec))
        })
        .collect::<Result<Vec<_>, EmbeddingError>>()?;

    items.sort_by_key(|(i, _)| *i);
    let vecs: Vec<Vec<f32>> = items.into_iter().map(|(_, emb)| emb).collect();

    // Validate: no empty vectors.
    for (i, v) in vecs.iter().enumerate() {
        if v.is_empty() {
            return Err(EmbeddingError::ParseError(format!(
                "embedding at index {i} is empty"
            )));
        }
    }

    Ok(vecs)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Parse a JSON array of numbers into `Vec<f32>`.
///
/// Rejects non-finite values (NaN, Infinity) that would corrupt downstream
/// cosine similarity computations.
fn parse_f32_array(value: &serde_json::Value) -> Result<Vec<f32>, EmbeddingError> {
    let arr = value
        .as_array()
        .ok_or_else(|| EmbeddingError::ParseError("embedding is not an array".to_owned()))?;
    arr.iter()
        .map(|v| {
            let f64_val = v.as_f64().ok_or_else(|| {
                EmbeddingError::ParseError("embedding value is not a number".to_owned())
            })?;
            let f32_val = f64_val as f32;
            if !f32_val.is_finite() {
                return Err(EmbeddingError::ParseError(format!(
                    "embedding value is not finite: {f64_val}"
                )));
            }
            Ok(f32_val)
        })
        .collect()
}

/// Map a `ureq::Error` to `EmbeddingError`, preserving the response body when available.
fn map_ureq_error(err: ureq::Error) -> EmbeddingError {
    EmbeddingError::HttpError(err.to_string())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Mock provider ──────────────────────────────────────────────────

    /// A mock provider for testing that returns predetermined embeddings.
    #[derive(Debug)]
    struct MockProvider {
        dim: usize,
        /// If set, embed() will return this error.
        error: Option<String>,
    }

    impl MockProvider {
        fn new(dim: usize) -> Self {
            Self { dim, error: None }
        }

        fn with_error(dim: usize, msg: &str) -> Self {
            Self {
                dim,
                error: Some(msg.to_owned()),
            }
        }
    }

    impl EmbeddingProvider for MockProvider {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            if let Some(ref msg) = self.error {
                return Err(EmbeddingError::HttpError(msg.clone()));
            }
            Ok(texts
                .iter()
                .enumerate()
                .map(|(i, _)| vec![i as f32 / 10.0; self.dim])
                .collect())
        }

        fn dimension(&self) -> usize {
            self.dim
        }
    }

    // ── Mock provider tests ────────────────────────────────────────────

    #[test]
    fn mock_provider_returns_expected_embeddings() {
        let provider = MockProvider::new(384);
        let texts = vec!["hello".to_owned(), "world".to_owned()];
        let result = provider.embed(&texts).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 384);
        assert_eq!(result[1].len(), 384);
        // First text → all 0.0, second text → all 0.1
        assert!((result[0][0] - 0.0).abs() < f32::EPSILON);
        assert!((result[1][0] - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn mock_provider_empty_input() {
        let provider = MockProvider::new(384);
        let result = provider.embed(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn mock_provider_dimension() {
        let provider = MockProvider::new(1536);
        assert_eq!(provider.dimension(), 1536);
    }

    #[test]
    fn mock_provider_error() {
        let provider = MockProvider::with_error(384, "connection refused");
        let result = provider.embed(&["test".to_owned()]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, EmbeddingError::HttpError(_)));
        assert!(err.to_string().contains("connection refused"));
    }

    // ── Config tests ───────────────────────────────────────────────────

    #[test]
    fn config_default() {
        let cfg = EmbeddingConfig::default();
        assert_eq!(cfg.provider, "ollama");
        assert!(cfg.model.is_empty());
        assert_eq!(cfg.dimension, 0);
        assert_eq!(cfg.batch_size, 32);
    }

    #[test]
    fn config_parse_ollama() {
        let toml_str = r#"
provider = "ollama"
model = "all-minilm"
dimension = 384
batch_size = 16
"#;
        let cfg: EmbeddingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.model, "all-minilm");
        assert_eq!(cfg.dimension, 384);
        assert_eq!(cfg.batch_size, 16);
    }

    #[test]
    fn config_parse_openai() {
        let toml_str = r#"
provider = "openai"
model = "text-embedding-3-small"
dimension = 1536
"#;
        let cfg: EmbeddingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.model, "text-embedding-3-small");
        assert_eq!(cfg.dimension, 1536);
        assert_eq!(cfg.batch_size, 32); // default
    }

    #[test]
    fn config_parse_partial_uses_defaults() {
        let toml_str = r#"
provider = "ollama"
"#;
        let cfg: EmbeddingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.provider, "ollama");
        assert!(cfg.model.is_empty());
        assert_eq!(cfg.dimension, 0);
        assert_eq!(cfg.batch_size, 32);
    }

    // ── Provider factory tests ─────────────────────────────────────────

    #[test]
    fn create_provider_unknown_returns_error() {
        let cfg = EmbeddingConfig {
            provider: "unknown_provider".to_owned(),
            ..Default::default()
        };
        let result = create_provider(&cfg);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, EmbeddingError::ConfigError(_)));
        assert!(err.to_string().contains("unknown embedding provider"));
        assert!(err.to_string().contains("unknown_provider"));
    }

    #[test]
    fn create_provider_ollama_succeeds() {
        let cfg = EmbeddingConfig {
            provider: "ollama".to_owned(),
            model: "all-minilm".to_owned(),
            dimension: 384,
            batch_size: 32,
            ..Default::default()
        };
        let provider = create_provider(&cfg).unwrap();
        assert_eq!(provider.dimension(), 384);
    }

    #[test]
    fn create_provider_ollama_defaults() {
        let cfg = EmbeddingConfig {
            provider: "ollama".to_owned(),
            ..Default::default()
        };
        let provider = create_provider(&cfg).unwrap();
        assert_eq!(provider.dimension(), 384); // default for all-minilm
    }

    #[test]
    fn create_provider_openai_missing_key() {
        // Empty api_key + no env var → error.
        let cfg = EmbeddingConfig {
            provider: "openai".to_owned(),
            api_key: String::new(),
            ..Default::default()
        };
        // This may succeed if the real OPENAI_API_KEY env var is set;
        // we only assert the error path when it's absent.
        if std::env::var("OPENAI_API_KEY").is_err() {
            let result = create_provider(&cfg);
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, EmbeddingError::ConfigError(_)));
            assert!(err.to_string().contains("OPENAI_API_KEY"));
        }
    }

    #[test]
    fn create_provider_openai_with_key() {
        // Pass the API key via config — no env var manipulation needed.
        let cfg = EmbeddingConfig {
            provider: "openai".to_owned(),
            api_key: "test-key-12345".to_owned(),
            ..Default::default()
        };
        let provider = create_provider(&cfg).unwrap();
        assert_eq!(provider.dimension(), 1536); // default for text-embedding-3-small
    }

    // ── Display impl test ──────────────────────────────────────────────

    #[test]
    fn config_display() {
        let cfg = EmbeddingConfig {
            provider: "ollama".to_owned(),
            model: "all-minilm".to_owned(),
            dimension: 384,
            batch_size: 32,
            ..Default::default()
        };
        let display = format!("{cfg}");
        assert!(display.contains("provider=ollama"));
        assert!(display.contains("model=all-minilm"));
        assert!(display.contains("dimension=384"));
        assert!(display.contains("batch_size=32"));
    }

    #[test]
    fn config_display_defaults() {
        let cfg = EmbeddingConfig::default();
        let display = format!("{cfg}");
        assert!(display.contains("model=(default)"));
        assert!(display.contains("dimension=(default)"));
    }

    // ── Error display tests ────────────────────────────────────────────

    #[test]
    fn error_display_messages() {
        let err = EmbeddingError::HttpError("timeout".to_owned());
        assert!(err.to_string().contains("timeout"));

        let err = EmbeddingError::StatusError {
            status: 429,
            body: "rate limited".to_owned(),
        };
        assert!(err.to_string().contains("429"));
        assert!(err.to_string().contains("rate limited"));

        let err = EmbeddingError::ParseError("bad json".to_owned());
        assert!(err.to_string().contains("bad json"));

        let err = EmbeddingError::DimensionMismatch {
            expected: 3,
            got: 2,
        };
        assert!(err.to_string().contains("3"));
        assert!(err.to_string().contains("2"));

        let err = EmbeddingError::ConfigError("missing key".to_owned());
        assert!(err.to_string().contains("missing key"));
    }

    // ── Provider trait object tests ────────────────────────────────────

    #[test]
    fn provider_as_trait_object() {
        let provider: Box<dyn EmbeddingProvider> = Box::new(MockProvider::new(384));
        assert_eq!(provider.dimension(), 384);
        let result = provider.embed(&["test".to_owned()]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 384);
    }

    #[test]
    fn provider_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OllamaProvider>();
        assert_send_sync::<OpenAIProvider>();
    }

    // ── JSON response parsing tests ────────────────────────────────────

    #[test]
    fn parse_ollama_response_valid() {
        let json = serde_json::json!({
            "embeddings": [[0.1, 0.2, 0.3], [0.4, 0.5, 0.6]]
        });
        let result = parse_ollama_response(&json, 2).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 3);
        assert!((result[0][0] - 0.1).abs() < f32::EPSILON);
        assert!((result[1][2] - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_ollama_response_count_mismatch() {
        let json = serde_json::json!({
            "embeddings": [[0.1, 0.2]]
        });
        let result = parse_ollama_response(&json, 2);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmbeddingError::CountMismatch {
                expected: 2,
                got: 1
            }
        ));
    }

    #[test]
    fn parse_ollama_response_missing_embeddings() {
        let json = serde_json::json!({"model": "test"});
        let result = parse_ollama_response(&json, 1);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EmbeddingError::ParseError(_)));
    }

    #[test]
    fn parse_openai_response_valid() {
        let json = serde_json::json!({
            "data": [
                {"embedding": [0.1, 0.2], "index": 0},
                {"embedding": [0.3, 0.4], "index": 1}
            ]
        });
        let result = parse_openai_response(&json, 2).unwrap();
        assert_eq!(result.len(), 2);
        assert!((result[0][0] - 0.1).abs() < f32::EPSILON);
        assert!((result[1][0] - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_openai_response_reorders_by_index() {
        let json = serde_json::json!({
            "data": [
                {"embedding": [0.3, 0.4], "index": 1},
                {"embedding": [0.1, 0.2], "index": 0}
            ]
        });
        let result = parse_openai_response(&json, 2).unwrap();
        // Should be reordered: index 0 first, index 1 second
        assert!((result[0][0] - 0.1).abs() < f32::EPSILON);
        assert!((result[1][0] - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn parse_openai_response_count_mismatch() {
        let json = serde_json::json!({
            "data": [{"embedding": [0.1], "index": 0}]
        });
        let result = parse_openai_response(&json, 2);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmbeddingError::CountMismatch {
                expected: 2,
                got: 1
            }
        ));
    }

    #[test]
    fn parse_openai_response_missing_data() {
        let json = serde_json::json!({"model": "test"});
        let result = parse_openai_response(&json, 1);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), EmbeddingError::ParseError(_)));
    }

    #[test]
    fn parse_f32_array_valid() {
        let json = serde_json::json!([1.0, 2.0, 3.0]);
        let result = parse_f32_array(&json).unwrap();
        assert_eq!(result, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn parse_f32_array_not_array() {
        let json = serde_json::json!("not an array");
        let result = parse_f32_array(&json);
        assert!(result.is_err());
    }

    #[test]
    fn parse_f32_array_non_number() {
        let json = serde_json::json!([1.0, "bad", 3.0]);
        let result = parse_f32_array(&json);
        assert!(result.is_err());
    }

    // ── Code review fixes: G1 tests ────────────────────────────────────

    #[test]
    fn create_provider_batch_size_zero_returns_error() {
        let cfg = EmbeddingConfig {
            batch_size: 0,
            ..Default::default()
        };
        let result = create_provider(&cfg);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, EmbeddingError::ConfigError(_)));
        assert!(err.to_string().contains("batch_size"));
    }

    #[test]
    fn create_provider_case_insensitive() {
        let cfg = EmbeddingConfig {
            provider: "Ollama".to_owned(),
            ..Default::default()
        };
        let provider = create_provider(&cfg).unwrap();
        assert_eq!(provider.dimension(), 384);

        let cfg2 = EmbeddingConfig {
            provider: " OLLAMA ".to_owned(),
            ..Default::default()
        };
        let provider2 = create_provider(&cfg2).unwrap();
        assert_eq!(provider2.dimension(), 384);
    }

    #[test]
    fn create_provider_whitespace_api_key_rejected() {
        let cfg = EmbeddingConfig {
            provider: "openai".to_owned(),
            api_key: "   ".to_owned(),
            ..Default::default()
        };
        // Only test when OPENAI_API_KEY env var is absent.
        if std::env::var("OPENAI_API_KEY").is_err() {
            let result = create_provider(&cfg);
            assert!(result.is_err());
        }
    }

    #[test]
    fn api_key_not_serialized() {
        let cfg = EmbeddingConfig {
            api_key: "secret-key-123".to_owned(),
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(
            !json.contains("secret-key-123"),
            "api_key leaked in serialization"
        );
        assert!(
            !json.contains("api_key"),
            "api_key field present in serialization"
        );
    }

    #[test]
    fn parse_ollama_error_response() {
        let json = serde_json::json!({"error": "model 'bad-model' not found"});
        let result = parse_ollama_response(&json, 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("bad-model"));
    }

    #[test]
    fn parse_openai_error_response() {
        let json =
            serde_json::json!({"error": {"message": "invalid api key", "type": "auth_error"}});
        let result = parse_openai_response(&json, 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid api key"));
    }

    #[test]
    fn parse_openai_response_missing_index_returns_error() {
        let json = serde_json::json!({
            "data": [
                {"embedding": [0.1, 0.2]},
                {"embedding": [0.3, 0.4], "index": 1}
            ]
        });
        let result = parse_openai_response(&json, 2);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("index"));
    }

    #[test]
    fn parse_f32_array_infinity_rejected() {
        // f64::MAX overflows f32 → f32::INFINITY → rejected
        let json = serde_json::json!([1.0, f64::MAX]);
        let result = parse_f32_array(&json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not finite"));
    }

    #[test]
    fn parse_ollama_response_empty_vector_rejected() {
        let json = serde_json::json!({"embeddings": [[0.1, 0.2], []]});
        let result = parse_ollama_response(&json, 2);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn count_mismatch_error_display() {
        let err = EmbeddingError::CountMismatch {
            expected: 3,
            got: 1,
        };
        assert!(err.to_string().contains("3"));
        assert!(err.to_string().contains("1"));
        assert!(err.to_string().contains("embedding vectors"));
    }

    #[test]
    fn dimension_mismatch_error_display() {
        let err = EmbeddingError::DimensionMismatch {
            expected: 384,
            got: 1536,
        };
        assert!(err.to_string().contains("384"));
        assert!(err.to_string().contains("1536"));
    }
}
