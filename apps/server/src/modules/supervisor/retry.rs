//! Stage 6 model retry classifier.
//!
//! Turns a `ModelStreamEvent::Failed { code, detail }` into a typed
//! `RetryDecision`. The Stage-4 rule in `interface.rs` (park anything that
//! looks transient on `waiting_for_model`) is replaced by this classifier plus
//! the Round-internal retry loop in `execute_round_attempted`.
//!
//! Retry-After heads are not surfaced by the stream layer in M4, so the
//! `retry_after_ms` comes from an exponential backoff schedule keyed off the
//! fault class. If a future stream change exposes a parsed header, the caller
//! can pass it through `RetryDecision::override_after`.

use std::time::Duration;

/// Fault class driving backoff and `waiting_for_model` parking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultClass {
    /// 401/403/disabled — provider/config problem. Skip to the next candidate;
    /// without one, fail the Turn immediately. Never retry in-place.
    Config,
    /// 429/503/timeout/transport — transient. Sleep `retry_after`, retry in
    /// place up to the candidate's attempt cap. After the cap, park the Turn on
    /// `waiting_for_model` rather than failing it.
    Transient,
    /// Anything else (malformed request, schema, unknown). Fail the Turn.
    Fatal,
}

/// Decision for one failed attempt.
#[derive(Debug, Clone)]
pub struct RetryDecision {
    pub class: FaultClass,
    /// Suggested delay before the next attempt of this candidate.
    pub retry_after: Duration,
}

/// Caps the in-Round retry loop: initial attempt plus 5 retries.
pub const MAX_ATTEMPTS_PER_CANDIDATE: usize = 6;

/// Classify a provider fault. `attempt` is 0-indexed within the current
/// candidate (0 = first failure of the first attempt). Transient faults use an
/// exponential backoff (1s, 2s, 4s, 8s, 16s); the schedule is bounded by the
/// same attempt cap so the longest wait follows the last retry.
pub fn classify(code: &str, detail: &str, attempt: usize) -> RetryDecision {
    let class = match code {
        "PROVIDER_AUTH_FAILED" => FaultClass::Config,
        "PROVIDER_STREAM_FAILED" | "PROVIDER_UNREACHABLE" | "PROVIDER_TIMEOUT" => {
            // Stream failures carry an HTTP status or transport word; classify
            // by detail so a 400 schema fault becomes fatal, not transient.
            if looks_config_fault(detail) {
                FaultClass::Config
            } else if looks_transient(detail) {
                FaultClass::Transient
            } else {
                FaultClass::Fatal
            }
        }
        _ => FaultClass::Fatal,
    };
    let backoff = match class {
        FaultClass::Config | FaultClass::Fatal => Duration::ZERO,
        FaultClass::Transient => {
            let exp = attempt.min(4);
            Duration::from_millis(1_000u64 * 2u64.pow(exp as u32))
        }
    };
    RetryDecision {
        class,
        retry_after: backoff,
    }
}

fn looks_transient(detail: &str) -> bool {
    let l = detail.to_ascii_lowercase();
    const TRANSIENT: &[&str] = &[
        "429",
        "503",
        "502",
        "504",
        "rate",
        "limit",
        "timeout",
        "temporar",
        "unavailable",
        "overloaded",
        "connect",
        "network",
        "transport",
        "provider unreachable",
    ];
    TRANSIENT.iter().any(|k| l.contains(k))
}

fn looks_config_fault(detail: &str) -> bool {
    let l = detail.to_ascii_lowercase();
    // 401/403 already arrive as PROVIDER_AUTH_FAILED (Config). Other config
    // faults surface as PROVIDER_STREAM_FAILED with an HTTP 4xx status word.
    const CONFIG: &[&str] = &[
        "http 400",
        "http 401",
        "http 403",
        "http 404",
        "http 422",
        "invalid",
        "unauthorized",
        "forbidden",
    ];
    CONFIG.iter().any(|k| l.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_failure_is_config_no_backoff() {
        let d = classify("PROVIDER_AUTH_FAILED", "provider rejected credentials", 0);
        assert_eq!(d.class, FaultClass::Config);
        assert_eq!(d.retry_after, Duration::ZERO);
    }

    #[test]
    fn rate_limit_is_transient_with_backoff() {
        let d = classify("PROVIDER_STREAM_FAILED", "provider HTTP 429", 0);
        assert_eq!(d.class, FaultClass::Transient);
        assert_eq!(d.retry_after, Duration::from_millis(1000));
        let d2 = classify("PROVIDER_STREAM_FAILED", "provider HTTP 429", 2);
        assert_eq!(d2.retry_after, Duration::from_millis(4000));
    }

    #[test]
    fn schema_400_is_config_not_transient() {
        let d = classify("PROVIDER_STREAM_FAILED", "provider HTTP 400", 0);
        assert_eq!(d.class, FaultClass::Config);
    }

    #[test]
    fn unknown_code_is_fatal() {
        let d = classify("SOMETHING_ELSE", "x", 0);
        assert_eq!(d.class, FaultClass::Fatal);
    }

    #[test]
    fn backoff_caps_at_16s() {
        let d = classify("PROVIDER_STREAM_FAILED", "provider HTTP 503", 9);
        assert_eq!(d.class, FaultClass::Transient);
        assert!(d.retry_after.as_millis() <= 16_000);
    }
}
