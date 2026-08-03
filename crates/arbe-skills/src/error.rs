use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed skill manifest at {path}: {reason}")]
    Malformed { path: String, reason: String },
}
