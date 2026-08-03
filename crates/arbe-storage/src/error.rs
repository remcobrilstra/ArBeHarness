use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to (de)serialize {path}: {source}")]
    Serde {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("session not found: {0}")]
    SessionNotFound(String),
}
