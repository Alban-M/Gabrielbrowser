use std::path::PathBuf;

/// Every fallible operation in Gabriel funnels through this type.
///
/// Errors carry enough context to be printed straight at a developer without a
/// wrapper sentence: the offending file, variable, or path is part of the message.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown variable `{0}` — define it in the environment, the vault, or pass --var {0}=…")]
    UnknownVariable(String),

    #[error("unknown secret `{0}` — add it with `gabriel vault set {0}`")]
    UnknownSecret(String),

    #[error("variable `{0}` expands into itself (recursion limit reached)")]
    VariableRecursion(String),

    #[error("unterminated `{{{{` in template: {0}")]
    UnterminatedTemplate(String),

    #[error("{path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: {message}")]
    Format { path: PathBuf, message: String },

    #[error("invalid JSON path `{0}`")]
    BadJsonPath(String),

    #[error("{0}")]
    Invalid(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }

    pub fn format(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Error::Format { path: path.into(), message: message.into() }
    }
}
