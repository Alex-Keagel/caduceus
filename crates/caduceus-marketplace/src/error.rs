use thiserror::Error;

#[derive(Debug, Error)]
pub enum MarketplaceError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("manifest parse error: {0}")]
    ManifestParse(String),
    #[error("plugin not found: {0}")]
    NotFound(String),
    #[error("invalid plugin: {0}")]
    Invalid(String),
}
