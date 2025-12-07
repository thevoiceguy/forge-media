//! Forge API - HTTP/WebSocket API for Forge Media Engine
//!
//! This crate provides the REST API and WebSocket interface for controlling
//! and monitoring the Forge media engine.

pub mod server;
pub mod routes;
pub mod error;
pub mod middleware;
pub mod response;

pub use server::ApiServer;
pub use error::{ApiError, ApiResult, ApiErrorResponse};
pub use response::{ApiResponse, ApiSuccess};
