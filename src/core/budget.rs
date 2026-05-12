//! Session-scoped token budget tracking with warning injection.
//!
//! Tracks cumulative tokens consumed in a session and warns when
//! approaching a configurable ceiling.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;

static TRACKER: OnceLock<BudgetTracker> = OnceLock::new();

const DEFAULT_BUDGET_TOKENS: u64 = 500_000;
const DEFAULT_WARN_PERCENT: u8 = 80;

pub struct BudgetTracker {
    output_tokens: AtomicU64,
    invocations: AtomicUsize,
    budget_limit: AtomicU64,
    warn_percent: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetStatus {
    Ok,
    Warning(u8), // current usage percent
    Exhausted,
}

impl BudgetTracker {
    fn new() -> Self {
        Self {
            output_tokens: AtomicU64::new(0),
            invocations: AtomicUsize::new(0),
            budget_limit: AtomicU64::new(DEFAULT_BUDGET_TOKENS),
            warn_percent: AtomicU64::new(DEFAULT_WARN_PERCENT as u64),
        }
    }

    pub fn global() -> &'static BudgetTracker {
        TRACKER.get_or_init(BudgetTracker::new)
    }

    pub fn configure(limit: u64, warn_pct: u8) {
        let tracker = Self::global();
        tracker.budget_limit.store(limit, Ordering::Relaxed);
        tracker
            .warn_percent
            .store(warn_pct as u64, Ordering::Relaxed);
    }

    pub fn record_output(&self, token_count: u64) {
        self.output_tokens.fetch_add(token_count, Ordering::Relaxed);
        self.invocations.fetch_add(1, Ordering::Relaxed);
    }

    pub fn status(&self) -> BudgetStatus {
        let used = self.output_tokens.load(Ordering::Relaxed);
        let limit = self.budget_limit.load(Ordering::Relaxed);
        let warn_pct = self.warn_percent.load(Ordering::Relaxed) as u8;

        if limit == 0 {
            return BudgetStatus::Ok;
        }

        let usage_pct = ((used as f64 / limit as f64) * 100.0) as u8;

        if usage_pct >= 100 {
            BudgetStatus::Exhausted
        } else if usage_pct >= warn_pct {
            BudgetStatus::Warning(usage_pct)
        } else {
            BudgetStatus::Ok
        }
    }

    pub fn tokens_used(&self) -> u64 {
        self.output_tokens.load(Ordering::Relaxed)
    }

    pub fn invocation_count(&self) -> usize {
        self.invocations.load(Ordering::Relaxed)
    }

    pub fn budget_limit(&self) -> u64 {
        self.budget_limit.load(Ordering::Relaxed)
    }
}

/// Approximate token count (whitespace split — same as rtk tracking uses).
pub fn estimate_tokens(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

/// Generate a warning suffix if budget threshold is breached.
pub fn warning_suffix() -> Option<String> {
    let tracker = BudgetTracker::global();
    match tracker.status() {
        BudgetStatus::Warning(pct) => Some(format!(
            "[rtk: {}% of session budget ({}/{})]",
            pct,
            tracker.tokens_used(),
            tracker.budget_limit()
        )),
        BudgetStatus::Exhausted => Some(format!(
            "[rtk: session budget exhausted ({}/{})]",
            tracker.tokens_used(),
            tracker.budget_limit()
        )),
        BudgetStatus::Ok => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens("hello world"), 2);
        assert_eq!(estimate_tokens("one two three four five"), 5);
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_budget_status_ok() {
        let tracker = BudgetTracker::new();
        tracker.budget_limit.store(1000, Ordering::Relaxed);
        tracker.output_tokens.store(500, Ordering::Relaxed);
        assert_eq!(tracker.status(), BudgetStatus::Ok);
    }

    #[test]
    fn test_budget_status_warning() {
        let tracker = BudgetTracker::new();
        tracker.budget_limit.store(1000, Ordering::Relaxed);
        tracker.warn_percent.store(80, Ordering::Relaxed);
        tracker.output_tokens.store(850, Ordering::Relaxed);
        assert_eq!(tracker.status(), BudgetStatus::Warning(85));
    }

    #[test]
    fn test_budget_status_exhausted() {
        let tracker = BudgetTracker::new();
        tracker.budget_limit.store(1000, Ordering::Relaxed);
        tracker.output_tokens.store(1100, Ordering::Relaxed);
        assert_eq!(tracker.status(), BudgetStatus::Exhausted);
    }

    #[test]
    fn test_record_output() {
        let tracker = BudgetTracker::new();
        tracker.record_output(100);
        tracker.record_output(200);
        assert_eq!(tracker.tokens_used(), 300);
        assert_eq!(tracker.invocation_count(), 2);
    }

    #[test]
    fn test_zero_budget_always_ok() {
        let tracker = BudgetTracker::new();
        tracker.budget_limit.store(0, Ordering::Relaxed);
        tracker.output_tokens.store(999999, Ordering::Relaxed);
        assert_eq!(tracker.status(), BudgetStatus::Ok);
    }
}
