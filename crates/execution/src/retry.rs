//! Model retry classifier.
//!
//! Turns a `ModelStreamEvent::Failed { code, detail }` into a typed
//! `RetryDecision` used by the Round-internal retry loop.
//! Retry-After headers are not surfaced by the stream layer yet, so the
//! `retry_after_ms` comes from an exponential backoff schedule keyed off the
//! fault class. If a future stream change exposes a parsed header, the caller
//! can pass it through `RetryDecision::override_after`.

use std::time::Duration;

/// Fault class driving backoff and the round's terminal posture.
///
/// Per the product spec ("Reconnecting (X/5): reason"), almost every provider
/// fault is retried in place with a short backoff so the user sees the attempt
/// counter climb instead of a silent immediate failure. Only faults we *know*
/// cannot succeed on a re-send are excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultClass {
    /// Credential / quota / invalid-request faults that a re-send cannot fix.
    /// Never retry in place; park the turn `waiting_for_model` (so the UI can
    /// surface the reason and offer a manual retry) or fail it.
    Config,
    /// Everything else — network blips, HTTP 5xx, timeouts, truncated streams,
    /// transport errors, and any fault we could not positively identify as
    /// deterministic. Retry in place up to the attempt cap before parking.
    Transient,
}

/// Decision for one failed attempt.
#[derive(Debug, Clone)]
pub struct RetryDecision {
    pub class: FaultClass,
    /// Suggested delay before the next attempt of this candidate.
    pub retry_after: Duration,
}

/// Caps the in-Round retry loop: one initial attempt plus five retries. The
/// `model_attempts.attempt_number` column is `CHECK BETWEEN 0 AND 5`, which pins
/// this cap — changing it requires a migration. The "X/5" surfaced to the UI
/// counts retries, so the retry index runs 1..=5.
pub const MAX_ATTEMPTS_PER_CANDIDATE: usize = 6;

/// Fixed 10-second backoff for all transient faults so the user sees the
/// attempt counter climb at a predictable cadence. Config faults never retry
/// (zero backoff); the class is the only meaningful signal for them.
fn retry_backoff(_attempt: usize) -> Duration {
    Duration::from_secs(10)
}

/// Classify a provider fault. `attempt` is the retry index we are about to make
/// (1 = first retry after the initial attempt failed). Config faults never
/// retry (zero backoff); everything else is Transient and retried with the
/// short schedule.
pub fn classify(code: &str, detail: &str, attempt: usize) -> RetryDecision {
    let class = classify_fault(code, detail);
    let retry_after = match class {
        FaultClass::Config => Duration::ZERO,
        FaultClass::Transient => retry_backoff(attempt),
    };
    RetryDecision { class, retry_after }
}

/// Classification only, without a backoff. Used when a caller needs the class
/// (to decide the terminal posture) but not the schedule.
pub fn classify_fault(code: &str, detail: &str) -> FaultClass {
    match code {
        // Hard credential rejection — re-sending identical headers cannot help.
        "PROVIDER_AUTH_FAILED" => FaultClass::Config,
        "PROVIDER_STREAM_FAILED" | "PROVIDER_UNREACHABLE" | "PROVIDER_TIMEOUT" => {
            // A streaming fault may carry an HTTP status or transport word in
            // its detail. Only a small set of HTTP statuses are known to be
            // retries-will-never-help (auth/quota/schema); everything else —
            // including HTTP 5xx, timeouts, truncations, and *unidentified*
            // statuses — is retryable per the "Reconnecting (X/5)" spec.
            if looks_config_fault(detail) {
                FaultClass::Config
            } else {
                FaultClass::Transient
            }
        }
        // Unknown error code: we cannot prove it is deterministic, and the
        // spec prefers retrying over failing fast, so retry.
        _ => FaultClass::Transient,
    }
}

fn looks_config_fault(detail: &str) -> bool {
    let l = detail.to_ascii_lowercase();
    /// Only responses a re-send provably cannot satisfy: bad credentials or a
    /// malformed/bad-request payload. Quota / rate-limit responses ARE retried
    /// (they resolve once the window resets), so they are intentionally absent.
    const CONFIG: &[&str] = &[
        "http 400",
        "http 401",
        "http 403",
        "http 404",
        "http 422",
        "unauthorized",
        "forbidden",
        "invalid_api_key",
        "invalid request",
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
    fn rate_limit_is_transient_with_fixed_10s() {
        // HTTP 429 is quota/rate, retried at a fixed 10-second interval.
        let d = classify("PROVIDER_STREAM_FAILED", "provider HTTP 429", 1);
        assert_eq!(d.class, FaultClass::Transient);
        assert_eq!(d.retry_after, Duration::from_secs(10));
        let d2 = classify("PROVIDER_STREAM_FAILED", "provider HTTP 429", 3);
        assert_eq!(d2.retry_after, Duration::from_secs(10));
    }

    #[test]
    fn schema_400_is_config_not_transient() {
        let d = classify("PROVIDER_STREAM_FAILED", "provider HTTP 400", 0);
        assert_eq!(d.class, FaultClass::Config);
    }

    #[test]
    fn unknown_code_is_transient() {
        // Per the "Reconnecting (X/5)" spec, an unidentified fault is retried
        // rather than failing the Turn outright.
        let d = classify("SOMETHING_ELSE", "x", 1);
        assert_eq!(d.class, FaultClass::Transient);
    }

    #[test]
    fn backoff_is_fixed_10s() {
        // All transient faults retry at a fixed 10-second interval.
        let d = classify("PROVIDER_STREAM_FAILED", "provider HTTP 503", 9);
        assert_eq!(d.class, FaultClass::Transient);
        assert_eq!(d.retry_after, Duration::from_secs(10));
    }
}
