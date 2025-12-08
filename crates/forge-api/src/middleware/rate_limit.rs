//! Rate limiting middleware

use axum::body::Body;
use axum::extract::Request;
use axum::http::{Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Rate limiter state
#[derive(Clone)]
pub struct RateLimiter {
    state: Arc<RwLock<RateLimiterState>>,
    requests_per_window: usize,
    window_duration: Duration,
}

struct RateLimiterState {
    clients: HashMap<IpAddr, ClientState>,
    last_cleanup: Instant,
}

struct ClientState {
    requests: Vec<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter
    ///
    /// # Arguments
    /// * `requests_per_window` - Maximum requests allowed per window
    /// * `window_duration` - Duration of the time window
    pub fn new(requests_per_window: usize, window_duration: Duration) -> Self {
        Self {
            state: Arc::new(RwLock::new(RateLimiterState {
                clients: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
            requests_per_window,
            window_duration,
        }
    }

    /// Check if a request should be allowed
    pub async fn check_rate_limit(&self, ip: IpAddr) -> bool {
        let mut state = self.state.write().await;
        let now = Instant::now();

        // Cleanup old entries every 5 minutes
        if now.duration_since(state.last_cleanup) > Duration::from_secs(300) {
            state.clients.retain(|_, client_state| {
                client_state.requests.iter().any(|&req_time| {
                    now.duration_since(req_time) < self.window_duration
                })
            });
            state.last_cleanup = now;
        }

        // Get or create client state
        let client_state = state.clients.entry(ip).or_insert(ClientState {
            requests: Vec::new(),
        });

        // Remove expired requests
        client_state.requests.retain(|&req_time| {
            now.duration_since(req_time) < self.window_duration
        });

        // Check if limit exceeded
        if client_state.requests.len() >= self.requests_per_window {
            return false;
        }

        // Record this request
        client_state.requests.push(now);
        true
    }
}

/// Rate limiting middleware
pub async fn rate_limit_middleware(
    request: Request,
    next: Next,
) -> Result<Response<Body>, impl IntoResponse> {
    // Extract IP address from socket or X-Forwarded-For header
    let ip = if let Some(forwarded_for) = request.headers().get("X-Forwarded-For") {
        forwarded_for
            .to_str()
            .ok()
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse::<IpAddr>().ok())
    } else {
        request
            .extensions()
            .get::<std::net::SocketAddr>()
            .map(|addr| addr.ip())
    };

    let ip = ip.unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

    // Get rate limiter from extensions (will be set by the server)
    let rate_limiter = request.extensions().get::<RateLimiter>().cloned();

    if let Some(limiter) = rate_limiter {
        if !limiter.check_rate_limit(ip).await {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "Too many requests, please try again later",
            ));
        }
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn test_rate_limiter() {
        let limiter = RateLimiter::new(3, Duration::from_secs(1));
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // First 3 requests should be allowed
        assert!(limiter.check_rate_limit(ip).await);
        assert!(limiter.check_rate_limit(ip).await);
        assert!(limiter.check_rate_limit(ip).await);

        // 4th request should be blocked
        assert!(!limiter.check_rate_limit(ip).await);

        // Wait for window to expire
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Should be allowed again
        assert!(limiter.check_rate_limit(ip).await);
    }
}
