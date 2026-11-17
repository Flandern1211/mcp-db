use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const DEFAULT_TOKEN_RATE_LIMIT: usize = 60;
pub const DEFAULT_TOKEN_RATE_WINDOW: Duration = Duration::from_secs(60);

/// Sliding-window per-token request limiter. In-memory state resets on restart.
pub struct TokenRateLimiter {
    limit: usize,
    window: Duration,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl TokenRateLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, token_id: &str) -> bool {
        self.check_at(token_id, Instant::now())
    }

    fn check_at(&self, token_id: &str, now: Instant) -> bool {
        let mut hits = match self.hits.lock() {
            Ok(hits) => hits,
            Err(_) => return false,
        };
        let entries = hits.entry(token_id.to_string()).or_default();
        while entries
            .front()
            .is_some_and(|instant| now.duration_since(*instant) >= self.window)
        {
            entries.pop_front();
        }
        if entries.len() >= self.limit {
            return false;
        }
        entries.push_back(now);
        true
    }
}

impl Default for TokenRateLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_TOKEN_RATE_LIMIT, DEFAULT_TOKEN_RATE_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_requests_over_the_window_limit() {
        let limiter = TokenRateLimiter::new(2, Duration::from_secs(60));
        assert!(limiter.check("token-a"));
        assert!(limiter.check("token-a"));
        assert!(!limiter.check("token-a"));
        assert!(limiter.check("token-b"));
    }

    #[test]
    fn exact_window_boundary_recovers_and_restart_resets() {
        let start = Instant::now();
        let limiter = TokenRateLimiter::new(1, Duration::from_secs(60));
        assert!(limiter.check_at("a", start));
        assert!(!limiter.check_at("a", start + Duration::from_secs(59)));
        assert!(limiter.check_at("a", start + Duration::from_secs(60)));
        assert!(TokenRateLimiter::default().check("a"));
    }
}
