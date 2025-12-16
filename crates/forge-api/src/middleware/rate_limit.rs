//! Rate limiting middleware

use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Rate limiter state
#[derive(Clone)]
pub struct RateLimiter {
    state: Arc<RwLock<RateLimiterState>>,
    requests_per_window: usize,
    window_duration: Duration,
    trusted_proxies: Arc<Vec<IpAddr>>,
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
    /// * `trusted_proxies` - List of proxy IPs allowed to set X-Forwarded-For
    pub fn new(
        requests_per_window: usize,
        window_duration: Duration,
        trusted_proxies: Vec<IpAddr>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(RateLimiterState {
                clients: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
            requests_per_window,
            window_duration,
            trusted_proxies: Arc::new(trusted_proxies),
        }
    }

    /// Check if a request should be allowed
    pub async fn check_rate_limit(&self, ip: IpAddr) -> bool {
        let mut state = self.state.write().await;
        let now = Instant::now();

        // Cleanup old entries every 5 minutes
        if now.duration_since(state.last_cleanup) > Duration::from_secs(300) {
            state.clients.retain(|_, client_state| {
                client_state
                    .requests
                    .iter()
                    .any(|&req_time| now.duration_since(req_time) < self.window_duration)
            });
            state.last_cleanup = now;
        }

        // Get or create client state
        let client_state = state.clients.entry(ip).or_insert(ClientState {
            requests: Vec::new(),
        });

        // Remove expired requests
        client_state
            .requests
            .retain(|&req_time| now.duration_since(req_time) < self.window_duration);

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
///
/// Extracts the client IP address for rate limiting. Uses socket address by default.
/// Only trusts X-Forwarded-For header if the request originates from a configured trusted proxy.
pub async fn rate_limit_middleware(
    ConnectInfo(socket_addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Result<Response<Body>, impl IntoResponse> {
    // Get rate limiter from extensions (will be set by the server)
    let rate_limiter = request.extensions().get::<RateLimiter>().cloned();

    // Default to socket address (always trustworthy)
    let mut client_ip = socket_addr.ip();

    // Only trust X-Forwarded-For if request is from a trusted proxy
    if let Some(limiter) = &rate_limiter {
        if limiter.trusted_proxies.contains(&client_ip) {
            // Request is from trusted proxy, check X-Forwarded-For header
            if let Some(forwarded_for) = request.headers().get("X-Forwarded-For") {
                if let Some(forwarded_ip) = forwarded_for
                    .to_str()
                    .ok()
                    .and_then(|s| s.split(',').next())
                    .and_then(|s| s.trim().parse::<IpAddr>().ok())
                {
                    client_ip = forwarded_ip;
                }
            }
        }
        // If not from trusted proxy, use socket address (already set above)
    }

    if let Some(limiter) = rate_limiter {
        if !limiter.check_rate_limit(client_ip).await {
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
        let limiter = RateLimiter::new(3, Duration::from_secs(1), Vec::new());
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
