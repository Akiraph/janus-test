//! UTC clock helpers.
//!
//! Millisecond RFC 3339 is shared by MongoDB document fields and public wire data.
//! Changing precision or offset formatting would break cross-boundary ordering.

use chrono::{DateTime, SecondsFormat, Utc};

pub fn now_utc() -> DateTime<Utc> {
    Utc::now()
}

pub fn now_utc_str() -> String {
    format_utc(now_utc())
}

pub fn format_utc(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}
