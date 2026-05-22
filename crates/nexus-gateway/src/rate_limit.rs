use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    max_commands: u32,
    window: Duration,
    user_commands: HashMap<String, Vec<Instant>>,
}

impl RateLimiter {
    pub fn new(max_commands: u32, window: Duration) -> Self {
        Self {
            max_commands,
            window,
            user_commands: HashMap::new(),
        }
    }

    pub fn check_and_record(&mut self, user_id: &str) -> bool {
        let now = Instant::now();
        let commands = self.user_commands.entry(user_id.to_string()).or_default();

        commands.retain(|&t| now.duration_since(t) < self.window);

        if commands.len() >= self.max_commands as usize {
            return false;
        }

        commands.push(now);
        true
    }

    pub fn remaining(&self, user_id: &str) -> u32 {
        let now = Instant::now();
        let commands = self.user_commands.get(user_id);
        let count = commands
            .map(|cmds| {
                cmds.iter()
                    .filter(|&&t| now.duration_since(t) < self.window)
                    .count()
            })
            .unwrap_or(0);
        self.max_commands.saturating_sub(count as u32)
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(10, Duration::from_secs(300))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_allows_under_threshold() {
        let mut limiter = RateLimiter::new(3, Duration::from_secs(60));

        assert!(limiter.check_and_record("user1"));
        assert!(limiter.check_and_record("user1"));
        assert!(limiter.check_and_record("user1"));
    }

    #[test]
    fn test_rate_limit_blocks_over_threshold() {
        let mut limiter = RateLimiter::new(2, Duration::from_secs(60));

        assert!(limiter.check_and_record("user1"));
        assert!(limiter.check_and_record("user1"));
        assert!(!limiter.check_and_record("user1"));
    }

    #[test]
    fn test_rate_limit_different_users() {
        let mut limiter = RateLimiter::new(1, Duration::from_secs(60));

        assert!(limiter.check_and_record("user1"));
        assert!(limiter.check_and_record("user2"));
        assert!(!limiter.check_and_record("user1"));
        assert!(!limiter.check_and_record("user2"));
    }

    #[test]
    fn test_rate_limit_remaining() {
        let mut limiter = RateLimiter::new(5, Duration::from_secs(60));

        assert_eq!(limiter.remaining("user1"), 5);
        limiter.check_and_record("user1");
        assert_eq!(limiter.remaining("user1"), 4);
        limiter.check_and_record("user1");
        limiter.check_and_record("user1");
        assert_eq!(limiter.remaining("user1"), 2);
    }
}
