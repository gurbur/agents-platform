use std::io;

use reqwest::StatusCode;

/// Every failure maps to one `IF-CLI-001` exit code.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    /// A local file or stream could not be read or written (`--body-file`,
    /// stdout). Grouped with usage and configuration errors.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("network error: {}", with_causes(.0))]
    Network(#[source] reqwest::Error),
    #[error("{code}: {message} (request {request_id})")]
    Server {
        status: StatusCode,
        code: String,
        message: String,
        request_id: String,
    },
    #[error("unexpected response {status}: {detail}")]
    UnexpectedResponse { status: StatusCode, detail: String },
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::Server { .. } | CliError::UnexpectedResponse { .. } => 1,
            CliError::Usage(_) | CliError::Io { .. } => 2,
            CliError::Network(_) => 3,
        }
    }

    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        CliError::Io {
            context: context.into(),
            source,
        }
    }
}

/// `reqwest::Error`'s own message omits the cause ("connection refused", ...).
fn with_causes(err: &(dyn std::error::Error + 'static)) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
