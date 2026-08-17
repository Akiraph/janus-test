//! Model retry classification and backoff.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// After this many reconnects the retry loop switches to low-frequency
/// backoff. This is deliberately a threshold, never an execution limit.
pub const RECONNECT_NOTICE_ATTEMPTS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultClass {
    Config,
    Transient,
}

#[derive(Debug, Clone)]
pub struct RetryDecision {
    pub class: FaultClass,
    pub retry_after: Duration,
}

fn retry_backoff(attempt: usize) -> Duration {
    let base_seconds = if attempt <= RECONNECT_NOTICE_ATTEMPTS {
        2
    } else {
        10_u64.saturating_add((attempt - RECONNECT_NOTICE_ATTEMPTS) as u64 * 5)
    };
    let jitter_limit_ms = if attempt <= RECONNECT_NOTICE_ATTEMPTS {
        1_000
    } else {
        5_000
    };
    let jitter_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_millis() % jitter_limit_ms)
        .unwrap_or(0);
    Duration::from_secs(base_seconds.min(60)) + Duration::from_millis(u64::from(jitter_ms))
}

pub fn classify(code: &str, detail: &str, attempt: usize) -> RetryDecision {
    let class = classify_fault(code, detail);
    let retry_after = match class {
        FaultClass::Config => Duration::ZERO,
        FaultClass::Transient => retry_backoff(attempt),
    };
    RetryDecision { class, retry_after }
}

pub fn classify_fault(code: &str, detail: &str) -> FaultClass {
    match code {
        "PROVIDER_AUTH_FAILED" => FaultClass::Config,
        "PROVIDER_STREAM_FAILED" | "PROVIDER_UNREACHABLE" | "PROVIDER_TIMEOUT" => {
            if looks_config_fault(detail) {
                FaultClass::Config
            } else {
                FaultClass::Transient
            }
        }
        _ => FaultClass::Transient,
    }
}

fn looks_config_fault(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    [
        "http 400",
        "http 401",
        "http 403",
        "http 404",
        "http 422",
        "unauthorized",
        "forbidden",
        "invalid_api_key",
        "invalid request",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_failure_is_config_without_backoff() {
        let decision = classify("PROVIDER_AUTH_FAILED", "provider rejected credentials", 0);
        assert_eq!(decision.class, FaultClass::Config);
        assert_eq!(decision.retry_after, Duration::ZERO);
    }

    #[test]
    fn transient_faults_remain_retryable_after_notice_threshold() {
        let decision = classify("PROVIDER_STREAM_FAILED", "provider HTTP 429", 6);
        assert_eq!(decision.class, FaultClass::Transient);
        assert!(decision.retry_after >= Duration::from_secs(10));
    }

    #[test]
    fn schema_error_is_config() {
        let decision = classify("PROVIDER_STREAM_FAILED", "provider HTTP 400", 0);
        assert_eq!(decision.class, FaultClass::Config);
    }

    #[test]
    fn unknown_error_is_transient() {
        let decision = classify("SOMETHING_ELSE", "x", 1);
        assert_eq!(decision.class, FaultClass::Transient);
    }

    #[test]
    fn backoff_has_a_low_frequency_cap() {
        let decision = classify("PROVIDER_STREAM_FAILED", "provider HTTP 503", 100);
        assert!(decision.retry_after < Duration::from_secs(66));
    }
}
