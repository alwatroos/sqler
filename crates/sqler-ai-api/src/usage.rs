// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use std::ops::AddAssign;

/// Token counts for a request.
///
/// Each `ChatEvent::Usage` carries an increment. A provider that receives
/// cumulative counts sends the difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Usage {
    /// Tokens in the prompt.
    pub input_tokens: u64,
    /// Tokens in the completion.
    pub output_tokens: u64,
}

impl Usage {
    /// Creates a usage value.
    #[must_use]
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
        }
    }

    /// Input plus output tokens, saturating.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Adds `other` to `self`, saturating.
    pub fn add(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Usage) {
        self.add(other);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_and_add() {
        let mut u = Usage::new(3, 4);
        assert_eq!(u.total(), 7);
        u.add(Usage::new(1, 1));
        assert_eq!(u, Usage::new(4, 5));
        u += Usage::new(1, 0);
        assert_eq!(u, Usage::new(5, 5));
    }

    #[test]
    fn saturates() {
        let mut u = Usage::new(u64::MAX, u64::MAX);
        u.add(Usage::new(1, 1));
        assert_eq!(u, Usage::new(u64::MAX, u64::MAX));
        assert_eq!(u.total(), u64::MAX);
        assert_eq!(Usage::default(), Usage::new(0, 0));
    }
}
