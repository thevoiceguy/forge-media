//! API response types

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

/// Standard API response wrapper
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApiResponse<T> {
    Success(ApiSuccess<T>),
    Error(super::error::ApiErrorResponse),
}

/// Success response format
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiSuccess<T> {
    pub status: String,
    pub data: T,
}

impl<T> ApiSuccess<T> {
    pub fn new(data: T) -> Self {
        Self {
            status: "ok".to_string(),
            data,
        }
    }
}

impl<T> IntoResponse for ApiSuccess<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}

/// Helper function to create success responses
pub fn success<T>(data: T) -> ApiSuccess<T> {
    ApiSuccess::new(data)
}

/// Helper function to create created responses
pub fn created<T>(data: T) -> Response
where
    T: Serialize,
{
    (StatusCode::CREATED, Json(ApiSuccess::new(data))).into_response()
}

/// Helper function to create no content responses
pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_success_response() {
        let response = ApiSuccess::new("test data");
        assert_eq!(response.status, "ok");
        assert_eq!(response.data, "test data");
    }
}
