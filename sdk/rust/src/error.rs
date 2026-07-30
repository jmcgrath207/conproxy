/// SDK error types.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("connection error: {0}")]
    Connection(String),
    #[error("request failed ({code}): {message}")]
    Request { code: u16, message: String },
    #[error("config error: {0}")]
    Config(String),
    #[error("timeout")]
    Timeout,
}

/// Map tonic status codes to HTTP-equivalent status codes.
pub(crate) fn tonic_status_to_http(code: tonic::Code) -> u16 {
    match code {
        tonic::Code::Ok => 200,
        tonic::Code::InvalidArgument => 400,
        tonic::Code::Unauthenticated => 401,
        tonic::Code::PermissionDenied => 403,
        tonic::Code::NotFound => 404,
        tonic::Code::ResourceExhausted => 429,
        tonic::Code::Internal => 500,
        tonic::Code::Unavailable => 503,
        tonic::Code::DeadlineExceeded => 504,
        _ => 500,
    }
}

pub(crate) fn from_tonic_status(status: tonic::Status) -> SdkError {
    let code = tonic_status_to_http(status.code());
    SdkError::Request {
        code,
        message: status.message().to_string(),
    }
}
