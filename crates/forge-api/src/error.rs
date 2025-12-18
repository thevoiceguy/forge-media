//! API error types

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// API error response format
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub status: String,
    pub error: ErrorDetails,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorDetails {
    pub code: String,
    pub message: String,
}

/// API errors
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Room not found: {0}")]
    RoomNotFound(String),

    #[error("Recording not found: {0}")]
    RecordingNotFound(String),

    #[error("WebRTC connection not found: {0}")]
    ConnectionNotFound(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Not acceptable: {0}")]
    NotAcceptable(String),

    #[error("Internal server error: {0}")]
    Internal(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Forbidden")]
    Forbidden,

    #[error("Too many requests")]
    TooManyRequests,
}

impl ApiError {
    /// Get the HTTP status code for this error
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::SessionNotFound(_)
            | Self::RoomNotFound(_)
            | Self::RecordingNotFound(_)
            | Self::ConnectionNotFound(_)
            | Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotAcceptable(_) => StatusCode::NOT_ACCEPTABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
        }
    }

    /// Get the error code string
    pub fn error_code(&self) -> String {
        match self {
            Self::SessionNotFound(_) => "session_not_found",
            Self::RoomNotFound(_) => "room_not_found",
            Self::RecordingNotFound(_) => "recording_not_found",
            Self::ConnectionNotFound(_) => "connection_not_found",
            Self::NotFound(_) => "not_found",
            Self::InvalidRequest(_) => "invalid_request",
            Self::NotAcceptable(_) => "not_acceptable",
            Self::Internal(_) => "internal_error",
            Self::ServiceUnavailable(_) => "service_unavailable",
            Self::Conflict(_) => "conflict",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::TooManyRequests => "too_many_requests",
        }
        .to_string()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let error_response = ApiErrorResponse {
            status: "error".to_string(),
            error: ErrorDetails {
                code: self.error_code(),
                message: self.to_string(),
            },
        };

        (status, Json(error_response)).into_response()
    }
}

impl From<forge_core::ForgeError> for ApiError {
    fn from(err: forge_core::ForgeError) -> Self {
        match err {
            forge_core::ForgeError::SessionNotFound(id) => Self::SessionNotFound(id),
            forge_core::ForgeError::RoomNotFound(id) => Self::RoomNotFound(id),
            forge_core::ForgeError::RecordingNotFound(id) => Self::RecordingNotFound(id),
            forge_core::ForgeError::InvalidConfig(msg) => Self::InvalidRequest(msg),
            _ => Self::Internal(err.to_string()),
        }
    }
}

/// Result type for API operations
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_status_codes() {
        assert_eq!(
            ApiError::SessionNotFound("test".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::InvalidRequest("test".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::Internal("test".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(
            ApiError::SessionNotFound("test".into()).error_code(),
            "session_not_found"
        );
        assert_eq!(
            ApiError::InvalidRequest("test".into()).error_code(),
            "invalid_request"
        );
    }
}
