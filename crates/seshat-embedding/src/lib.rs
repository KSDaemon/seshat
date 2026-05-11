//! # seshat-embedding
//!
//! Embedding provider abstraction with a built-in local provider for Seshat.
//!
//! When the `[embedding]` section is present in `seshat.toml` **and** the
//! crate is compiled with the `builtin-embeddings` feature (enabled by
//! default), embeddings are generated locally using `fastembed-rs`
//! (all-MiniLM-L6-v2 model, 384 dimensions) — no external services needed.
//!
//! When the section is absent or the feature is disabled, all embedding code
//! is compiled away with zero overhead.
//!
//! ## Configuration
//!
//! ```toml
//! # seshat.toml — uncomment to enable vector search
//! # [embedding]
//! # model = ""          # empty → provider default (all-MiniLM-L6-v2)
//! # dimension = 0       # 0     → provider default (384)
//! # batch_size = 32
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

// ─── Error types ─────────────────────────────────────────────────────────────

/// Errors from embedding operations.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    /// Embedding provider failed to generate embeddings.
    #[error("embedding provider error: {0}")]
    ProviderError(String),

    /// Failed to parse or validate embedding output.
    #[error("failed to parse embedding response: {0}")]
    ParseError(String),

    /// The provider returned an unexpected number of embedding vectors.
    #[error("expected {expected} embedding vectors, got {got}")]
    CountMismatch { expected: usize, got: usize },

    /// An embedding vector has an unexpected number of dimensions.
    #[error("expected {expected}-dimensional embedding, got {got} dimensions")]
    DimensionMismatch { expected: usize, got: usize },

    /// Configuration error (e.g., invalid model name).
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
/// When present, the built-in provider is used (requires `builtin-embeddings`
/// feature, which is enabled by default).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct EmbeddingConfig {
    /// Model name. Empty string uses the provider default (all-MiniLM-L6-v2).
    pub model: String,
    /// Embedding vector dimension. `0` uses the provider default (384).
    pub dimension: usize,
    /// Batch size for embedding generation. Must be ≥ 1.
    pub batch_size: usize,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model: String::new(), // empty → provider default
            dimension: 0,         // 0     → provider default
            batch_size: 32,
        }
    }
}

impl fmt::Display for EmbeddingConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "model={}, dimension={}, batch_size={}",
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

/// Create the built-in embedding provider from configuration.
///
/// Requires the `builtin-embeddings` feature (enabled by default).
///
/// # Errors
///
/// Returns [`EmbeddingError::ConfigError`] if `batch_size` is 0 or if the
/// built-in provider fails to initialise.
pub fn create_provider(
    config: &EmbeddingConfig,
) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
    // Validate batch_size early — 0 would panic in `slice::chunks()`.
    if config.batch_size == 0 {
        return Err(EmbeddingError::ConfigError(
            "batch_size must be at least 1".to_owned(),
        ));
    }

    #[cfg(feature = "builtin-embeddings")]
    {
        builtin::create_builtin_provider(config)
    }

    #[cfg(not(feature = "builtin-embeddings"))]
    {
        Err(EmbeddingError::ConfigError(
            "embedding support is not compiled in — rebuild with the \
             'builtin-embeddings' feature (enabled by default)"
                .to_owned(),
        ))
    }
}

// ─── Built-in provider ───────────────────────────────────────────────────────

#[cfg(feature = "builtin-embeddings")]
mod builtin {
    use std::sync::Mutex;

    use super::*;

    /// Default model for the built-in provider.
    pub const DEFAULT_MODEL: &str = "all-MiniLM-L6-v2";
    /// Default embedding dimension for all-MiniLM-L6-v2.
    pub const DEFAULT_DIMENSION: usize = 384;

    pub fn create_builtin_provider(
        config: &EmbeddingConfig,
    ) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
        let model = if config.model.is_empty() {
            DEFAULT_MODEL.to_owned()
        } else {
            config.model.clone()
        };
        let dimension = if config.dimension == 0 {
            DEFAULT_DIMENSION
        } else {
            config.dimension
        };
        Ok(Box::new(BuiltinProvider::new(model, dimension)?))
    }

    /// Built-in embedding provider using fastembed-rs (all-MiniLM-L6-v2).
    ///
    /// Runs fully locally — no network calls, no external services.
    /// The model is bundled with the binary when `builtin-embeddings` feature
    /// is enabled.
    pub struct BuiltinProvider {
        model_name: String,
        dimension: usize,
        // fastembed 5 requires `&mut self` on `embed`. Wrap in `Mutex` so the
        // public `EmbeddingProvider::embed(&self, …)` API stays unchanged and
        // callers can keep sharing `Arc<dyn EmbeddingProvider>`.
        inner: Mutex<fastembed::TextEmbedding>,
    }

    impl fmt::Debug for BuiltinProvider {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("BuiltinProvider")
                .field("model_name", &self.model_name)
                .field("dimension", &self.dimension)
                .finish_non_exhaustive()
        }
    }

    impl BuiltinProvider {
        pub fn new(model_name: String, dimension: usize) -> Result<Self, EmbeddingError> {
            use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

            // Resolve fastembed model from name.
            let model = match model_name.as_str() {
                "all-MiniLM-L6-v2" => EmbeddingModel::AllMiniLML6V2,
                other => {
                    return Err(EmbeddingError::ConfigError(format!(
                        "unknown built-in model '{other}'. \
                         Supported: all-MiniLM-L6-v2"
                    )));
                }
            };

            // Suppress fastembed's own download progress output — seshat manages its own UI.
            let init_opts = InitOptions::new(model).with_show_download_progress(false);

            tracing::info!(model = %model_name, "Loading built-in embedding model (may download on first run)");

            let inner = TextEmbedding::try_new(init_opts)
                .map_err(|e| EmbeddingError::ProviderError(format!("failed to load model: {e}")))?;

            Ok(Self {
                model_name,
                dimension,
                inner: Mutex::new(inner),
            })
        }
    }

    impl EmbeddingProvider for BuiltinProvider {
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }

            let embeddings = {
                let mut model = self.inner.lock().map_err(|e| {
                    EmbeddingError::ProviderError(format!("model lock poisoned: {e}"))
                })?;
                model
                    .embed(texts, None)
                    .map_err(|e| EmbeddingError::ProviderError(e.to_string()))?
            };

            if embeddings.len() != texts.len() {
                return Err(EmbeddingError::CountMismatch {
                    expected: texts.len(),
                    got: embeddings.len(),
                });
            }

            // Validate: no empty, non-finite, or wrong-dimension vectors.
            for (i, vec) in embeddings.iter().enumerate() {
                if vec.is_empty() {
                    return Err(EmbeddingError::ParseError(format!(
                        "embedding at index {i} is empty"
                    )));
                }
                // Validate actual dimension matches configured dimension.
                // This catches misconfigured dimension= values before they
                // silently corrupt the vector store.
                if self.dimension > 0 && vec.len() != self.dimension {
                    return Err(EmbeddingError::DimensionMismatch {
                        expected: self.dimension,
                        got: vec.len(),
                    });
                }
                for &val in vec {
                    if !val.is_finite() {
                        return Err(EmbeddingError::ParseError(format!(
                            "embedding at index {i} contains non-finite value: {val}"
                        )));
                    }
                }
            }

            Ok(embeddings)
        }

        fn dimension(&self) -> usize {
            self.dimension
        }
    }
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
                return Err(EmbeddingError::ProviderError(msg.clone()));
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

    #[test]
    fn mock_provider_returns_expected_embeddings() {
        let provider = MockProvider::new(384);
        let texts = vec!["hello".to_owned(), "world".to_owned()];
        let result = provider.embed(&texts).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].len(), 384);
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
        let provider = MockProvider::with_error(384, "model load failed");
        let result = provider.embed(&["test".to_owned()]);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            EmbeddingError::ProviderError(_)
        ));
    }

    // ── Config tests ───────────────────────────────────────────────────

    #[test]
    fn config_default() {
        let cfg = EmbeddingConfig::default();
        assert!(cfg.model.is_empty());
        assert_eq!(cfg.dimension, 0);
        assert_eq!(cfg.batch_size, 32);
    }

    #[test]
    fn config_parse_minimal() {
        let toml_str = r#"batch_size = 16"#;
        let cfg: EmbeddingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.batch_size, 16);
        assert!(cfg.model.is_empty());
        assert_eq!(cfg.dimension, 0);
    }

    #[test]
    fn config_parse_full() {
        let toml_str = r#"
model = "all-MiniLM-L6-v2"
dimension = 384
batch_size = 64
"#;
        let cfg: EmbeddingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.model, "all-MiniLM-L6-v2");
        assert_eq!(cfg.dimension, 384);
        assert_eq!(cfg.batch_size, 64);
    }

    #[test]
    fn config_parse_empty_uses_defaults() {
        let cfg: EmbeddingConfig = toml::from_str("").unwrap();
        assert!(cfg.model.is_empty());
        assert_eq!(cfg.dimension, 0);
        assert_eq!(cfg.batch_size, 32);
    }

    // ── Display tests ──────────────────────────────────────────────────

    #[test]
    fn config_display_with_values() {
        let cfg = EmbeddingConfig {
            model: "all-MiniLM-L6-v2".to_owned(),
            dimension: 384,
            batch_size: 32,
        };
        let s = format!("{cfg}");
        assert!(s.contains("model=all-MiniLM-L6-v2"));
        assert!(s.contains("dimension=384"));
        assert!(s.contains("batch_size=32"));
    }

    #[test]
    fn config_display_defaults() {
        let cfg = EmbeddingConfig::default();
        let s = format!("{cfg}");
        assert!(s.contains("model=(default)"));
        assert!(s.contains("dimension=(default)"));
    }

    // ── Factory tests ──────────────────────────────────────────────────

    #[test]
    fn create_provider_batch_size_zero_returns_error() {
        let cfg = EmbeddingConfig {
            batch_size: 0,
            ..Default::default()
        };
        let result = create_provider(&cfg);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("batch_size"));
    }

    // ── Error display tests ────────────────────────────────────────────

    #[test]
    fn error_display_messages() {
        let err = EmbeddingError::ProviderError("model load failed".to_owned());
        assert!(err.to_string().contains("model load failed"));

        let err = EmbeddingError::ParseError("bad data".to_owned());
        assert!(err.to_string().contains("bad data"));

        let err = EmbeddingError::CountMismatch {
            expected: 3,
            got: 1,
        };
        assert!(err.to_string().contains("3"));
        assert!(err.to_string().contains("embedding vectors"));

        let err = EmbeddingError::DimensionMismatch {
            expected: 384,
            got: 1536,
        };
        assert!(err.to_string().contains("384"));
        assert!(err.to_string().contains("1536"));

        let err = EmbeddingError::ConfigError("bad config".to_owned());
        assert!(err.to_string().contains("bad config"));
    }

    // ── Trait object tests ─────────────────────────────────────────────

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
        assert_send_sync::<MockProvider>();
    }

    #[test]
    fn config_display_custom_model() {
        let cfg = EmbeddingConfig {
            model: "custom-model".to_owned(),
            dimension: 768,
            batch_size: 64,
        };
        let s = format!("{cfg}");
        assert!(s.contains("custom-model"));
        assert!(s.contains("dimension=768"));
        assert!(s.contains("batch_size=64"));
    }

    #[test]
    fn config_display_zero_dimension() {
        let cfg = EmbeddingConfig {
            dimension: 0,
            ..Default::default()
        };
        let s = format!("{cfg}");
        assert!(s.contains("dimension=(default)"));
    }

    #[test]
    fn mock_provider_debug() {
        let provider = MockProvider::new(128);
        let dbg = format!("{provider:?}");
        assert!(dbg.contains("MockProvider"));
    }

    #[test]
    fn error_display_count_mismatch() {
        let err = EmbeddingError::CountMismatch {
            expected: 10,
            got: 5,
        };
        let s = err.to_string();
        assert!(s.contains("10"));
        assert!(s.contains("5"));
    }

    #[test]
    fn error_display_dimension_mismatch() {
        let err = EmbeddingError::DimensionMismatch {
            expected: 512,
            got: 384,
        };
        let s = err.to_string();
        assert!(s.contains("512"));
        assert!(s.contains("384"));
        assert!(s.contains("dimension"));
    }

    #[test]
    fn mock_provider_zero_dimension() {
        let provider = MockProvider::new(0);
        assert_eq!(provider.dimension(), 0);
        let result = provider.embed(&["test".to_owned()]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 0);
    }

    #[test]
    fn mock_provider_embedding_values() {
        let provider = MockProvider::new(3);
        let texts = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let result = provider.embed(&texts).unwrap();
        assert_eq!(result.len(), 3);
        assert!((result[0][0] - 0.0).abs() < f32::EPSILON);
        assert!((result[1][0] - 0.1).abs() < f32::EPSILON);
        assert!((result[2][0] - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn create_provider_valid_config() {
        let cfg = EmbeddingConfig::default();
        let _ = create_provider(&cfg);
    }

    #[test]
    fn embedding_error_display_parse_error() {
        let err = EmbeddingError::ParseError("json malformed".to_owned());
        assert!(err.to_string().contains("json malformed"));
    }

    #[test]
    fn embedding_error_display_config_error() {
        let err = EmbeddingError::ConfigError("unsupported provider".to_owned());
        assert!(err.to_string().contains("unsupported provider"));
    }
}
