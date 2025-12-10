//! Forge API - HTTP/WebSocket API for Forge Media Engine
//!
//! This crate provides the REST API and WebSocket interface for controlling
//! and monitoring the Forge media engine.

pub mod error;
pub mod middleware;
pub mod response;
pub mod routes;
pub mod server;

pub use error::{ApiError, ApiErrorResponse, ApiResult};
pub use response::{ApiResponse, ApiSuccess};
pub use server::ApiServer;
