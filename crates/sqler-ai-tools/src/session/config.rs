// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Session limits, the retry policy and the session configuration.

use std::time::Duration;

use crate::session::prompt::PromptKind;

/// How a failed model step is retried. Only `RateLimited`, `Overloaded` and `Network` errors that arrive
/// before any text or tool call are retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// The most retries of one model step (default 2).
    pub max_retries: u32,
    /// The delay before the first retry; it doubles each time (default 1 s).
    pub base_delay: Duration,
    /// The longest delay, also the longest `retry_after` that is honoured (default 10 s).
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    /// The wait before retry number `attempt` (counted from 1), or `None` for no retry.
    ///
    /// A `retry_after` from the provider is used as is when it is at most `max_delay`; a longer
    /// one means no retry. Without it the delay is `base_delay * 2^(attempt-1)`, capped at
    /// `max_delay`. The caller checks `max_retries`.
    #[must_use]
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Option<Duration> {
        if let Some(after) = retry_after {
            return (after <= self.max_delay).then_some(after);
        }
        let shift = attempt.saturating_sub(1);
        let factor = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
        Some(self.base_delay.saturating_mul(factor).min(self.max_delay))
    }
}

/// The bounds of a session. All but `token_budget` apply to one user turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionLimits {
    /// The most model steps in one user turn (default 10).
    pub max_model_steps: u32,
    /// The most tool calls run in one user turn (default 25).
    pub max_tool_calls: u32,
    /// The most tokens (input plus output) over the whole session (default 200 000).
    pub token_budget: u64,
    /// The wall-clock limit of one user turn, not counting approval waits (default 300 s).
    pub turn_timeout: Duration,
    /// How often the same call may run in one user turn before it counts as a loop (default 2).
    pub max_identical_calls: u32,
    /// The retry policy of model steps.
    pub retry: RetryPolicy,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            max_model_steps: 10,
            max_tool_calls: 25,
            token_budget: 200_000,
            turn_timeout: Duration::from_secs(300),
            max_identical_calls: 2,
            retry: RetryPolicy::default(),
        }
    }
}

/// What a session needs to talk to the model.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SessionConfig {
    /// The model id.
    pub model: String,
    /// The bounds.
    pub limits: SessionLimits,
    /// The cap on output tokens of one model step.
    pub max_output_tokens: Option<u32>,
    /// The sampling temperature.
    pub temperature: Option<f32>,
    /// Which system prompt the session gets.
    pub prompt: PromptKind,
}

impl SessionConfig {
    /// A configuration for `model` with default limits.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            limits: SessionLimits::default(),
            max_output_tokens: None,
            temperature: None,
            prompt: PromptKind::Tab,
        }
    }

    /// The same configuration with another system prompt.
    #[must_use]
    pub fn with_prompt(mut self, prompt: PromptKind) -> Self {
        self.prompt = prompt;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let l = SessionLimits::default();
        assert_eq!(l.max_model_steps, 10);
        assert_eq!(l.max_tool_calls, 25);
        assert_eq!(l.token_budget, 200_000);
        assert_eq!(l.turn_timeout, Duration::from_secs(300));
        assert_eq!(l.max_identical_calls, 2);
        assert_eq!(l.retry.max_retries, 2);
        let c = SessionConfig::new("m");
        assert_eq!(c.model, "m");
        assert_eq!(c.limits, l);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay(1, None), Some(Duration::from_secs(1)));
        assert_eq!(p.delay(2, None), Some(Duration::from_secs(2)));
        assert_eq!(p.delay(3, None), Some(Duration::from_secs(4)));
        assert_eq!(p.delay(5, None), Some(Duration::from_secs(10)));
        assert_eq!(p.delay(200, None), Some(Duration::from_secs(10)));
    }

    #[test]
    fn retry_after_wins_unless_too_long() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(3))),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(10))),
            Some(Duration::from_secs(10))
        );
        assert_eq!(p.delay(1, Some(Duration::from_secs(11))), None);
    }
}
