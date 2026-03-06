use rv_types::{VtimDur, VtimReal};

use crate::message::HttpMessage;

/// Parsed Cache-Control directives.
/// Based on cache_rfc2616.c parsing logic.
#[derive(Debug, Clone, Default)]
pub struct CacheControl {
    pub max_age: Option<f64>,
    pub s_maxage: Option<f64>,
    pub no_cache: bool,
    pub no_store: bool,
    pub must_revalidate: bool,
    pub proxy_revalidate: bool,
    pub public: bool,
    pub private: bool,
    pub immutable: bool,
    pub stale_while_revalidate: Option<f64>,
    pub stale_if_error: Option<f64>,
}

impl CacheControl {
    /// Parse Cache-Control header value.
    pub fn parse(header_value: &str) -> Self {
        let mut cc = Self::default();

        for directive in header_value.split(',') {
            let directive = directive.trim();
            let (key, value) = match directive.split_once('=') {
                Some((k, v)) => (k.trim(), Some(v.trim().trim_matches('"'))),
                None => (directive, None),
            };

            match key.to_ascii_lowercase().as_str() {
                "max-age" => cc.max_age = value.and_then(|v| v.parse().ok()),
                "s-maxage" => cc.s_maxage = value.and_then(|v| v.parse().ok()),
                "no-cache" => cc.no_cache = true,
                "no-store" => cc.no_store = true,
                "must-revalidate" => cc.must_revalidate = true,
                "proxy-revalidate" => cc.proxy_revalidate = true,
                "public" => cc.public = true,
                "private" => cc.private = true,
                "immutable" => cc.immutable = true,
                "stale-while-revalidate" => {
                    cc.stale_while_revalidate = value.and_then(|v| v.parse().ok())
                }
                "stale-if-error" => cc.stale_if_error = value.and_then(|v| v.parse().ok()),
                _ => {}
            }
        }

        cc
    }

    /// Whether the response is explicitly uncacheable.
    pub fn is_uncacheable(&self) -> bool {
        self.no_store || self.private
    }
}

/// TTL calculation result.
/// Mirrors the logic from RFC2616_Ttl in cache_rfc2616.c
#[derive(Debug, Clone)]
pub struct TtlCalculation {
    pub ttl: VtimDur,
    pub grace: VtimDur,
    pub keep: VtimDur,
    pub t_origin: VtimReal,
    pub uncacheable: bool,
}

impl TtlCalculation {
    /// Calculate TTL from a backend response, following RFC 2616/7234 rules.
    /// Based on RFC2616_Ttl() in cache_rfc2616.c
    ///
    /// Priority order:
    /// 1. s-maxage (for shared caches)
    /// 2. max-age
    /// 3. Expires header
    /// 4. Default TTL
    pub fn from_response(
        resp: &HttpMessage,
        now: VtimReal,
        default_ttl: VtimDur,
        default_grace: VtimDur,
        default_keep: VtimDur,
    ) -> Self {
        let mut result = Self {
            ttl: default_ttl,
            grace: default_grace,
            keep: default_keep,
            t_origin: now,
            uncacheable: false,
        };

        // Parse Cache-Control
        let cc = resp
            .get_header("Cache-Control")
            .map(CacheControl::parse)
            .unwrap_or_default();

        // Check for uncacheable directives
        if cc.is_uncacheable() {
            result.uncacheable = true;
            result.ttl = VtimDur::from_secs(-1.0);
            return result;
        }

        // Parse Date header for t_origin
        if let Some(date_str) = resp.get_header("Date")
            && let Some(date) = parse_http_date(date_str)
        {
            result.t_origin = date;
        }

        // Parse Age header
        let age: f64 = resp
            .get_header("Age")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0.0);

        // s-maxage takes priority (for shared caches like Varnish)
        if let Some(s_maxage) = cc.s_maxage {
            result.ttl = VtimDur::from_secs(s_maxage - age);
            return result;
        }

        // max-age next
        if let Some(max_age) = cc.max_age {
            result.ttl = VtimDur::from_secs(max_age - age);
            return result;
        }

        // Expires header
        if let Some(expires_str) = resp.get_header("Expires")
            && let Some(expires) = parse_http_date(expires_str)
        {
            let ttl_secs = expires.as_secs() - result.t_origin.as_secs();
            result.ttl = VtimDur::from_secs(ttl_secs);
            return result;
        }

        // Check for no-cache (cacheable but must revalidate)
        if cc.no_cache {
            result.ttl = VtimDur::from_secs(0.0);
            return result;
        }

        // Stale-while-revalidate can override grace
        if let Some(swr) = cc.stale_while_revalidate {
            result.grace = VtimDur::from_secs(swr);
        }

        result
    }
}

/// Check if a response is a conditional response (304 Not Modified).
/// Based on RFC2616_Do_Cond in cache_rfc2616.c
pub fn evaluate_conditional(
    req: &HttpMessage,
    resp_etag: Option<&str>,
    resp_last_modified: Option<&str>,
) -> bool {
    // If-None-Match (ETag comparison)
    if let Some(inm) = req.get_header("If-None-Match") {
        if let Some(etag) = resp_etag {
            // Strong comparison for If-None-Match
            if inm.trim() == "*" || etag_matches(inm, etag) {
                return true;
            }
        }
        return false;
    }

    // If-Modified-Since
    if let Some(ims) = req.get_header("If-Modified-Since")
        && let Some(lm) = resp_last_modified
        && let (Some(ims_time), Some(lm_time)) = (parse_http_date(ims), parse_http_date(lm))
        && lm_time.as_secs() <= ims_time.as_secs()
    {
        return true;
    }

    false
}

/// Check if any ETag in the If-None-Match header matches.
fn etag_matches(inm_header: &str, etag: &str) -> bool {
    for candidate in inm_header.split(',') {
        let candidate = candidate.trim();
        // Weak comparison: strip W/ prefix
        let candidate_clean = candidate.strip_prefix("W/").unwrap_or(candidate);
        let etag_clean = etag.strip_prefix("W/").unwrap_or(etag);
        if candidate_clean == etag_clean {
            return true;
        }
    }
    false
}

/// Parse an HTTP date string (RFC 2616 section 3.3).
/// Supports:
/// - RFC 1123: "Sun, 06 Nov 1994 08:49:37 GMT"
/// - RFC 850:  "Sunday, 06-Nov-94 08:49:37 GMT"
/// - asctime:  "Sun Nov  6 08:49:37 1994"
pub fn parse_http_date(s: &str) -> Option<VtimReal> {
    let s = s.trim();

    // Try RFC 1123 format
    if let Some(t) = parse_rfc1123(s) {
        return Some(t);
    }

    // Try RFC 850 format
    if let Some(t) = parse_rfc850(s) {
        return Some(t);
    }

    // Try asctime format
    parse_asctime(s)
}

fn month_number(month: &str) -> Option<u32> {
    match month {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

/// Convert date components to Unix timestamp (simplified).
fn components_to_timestamp(year: u32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> f64 {
    // Simplified date-to-timestamp calculation
    let mut y = year as i64;
    let mut m = month as i64;

    // Adjust for months before March
    if m <= 2 {
        y -= 1;
        m += 12;
    }

    let days = 365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + day as i64 - 719469;

    (days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64) as f64
}

/// Parse RFC 1123 date: "Sun, 06 Nov 1994 08:49:37 GMT"
fn parse_rfc1123(s: &str) -> Option<VtimReal> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }

    let day: u32 = parts[1].parse().ok()?;
    let month = month_number(parts[2])?;
    let year: u32 = parts[3].parse().ok()?;
    let time_parts: Vec<&str> = parts[4].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;

    Some(VtimReal::from_secs(components_to_timestamp(
        year, month, day, hour, min, sec,
    )))
}

/// Parse RFC 850 date: "Sunday, 06-Nov-94 08:49:37 GMT"
fn parse_rfc850(s: &str) -> Option<VtimReal> {
    let (_, rest) = s.split_once(", ")?;
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 3 || parts[2] != "GMT" {
        return None;
    }

    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let day: u32 = date_parts[0].parse().ok()?;
    let month = month_number(date_parts[1])?;
    let mut year: u32 = date_parts[2].parse().ok()?;
    if year < 100 {
        year += if year < 70 { 2000 } else { 1900 };
    }

    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;

    Some(VtimReal::from_secs(components_to_timestamp(
        year, month, day, hour, min, sec,
    )))
}

/// Parse asctime date: "Sun Nov  6 08:49:37 1994"
fn parse_asctime(s: &str) -> Option<VtimReal> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }

    let month = month_number(parts[1])?;
    let day: u32 = parts[2].parse().ok()?;
    let year: u32 = parts[4].parse().ok()?;

    let time_parts: Vec<&str> = parts[3].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;

    Some(VtimReal::from_secs(components_to_timestamp(
        year, month, day, hour, min, sec,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::HttpVersion;
    use rv_types::HttpStatus;

    #[test]
    fn test_cache_control_parse() {
        let cc = CacheControl::parse("max-age=300, public");
        assert_eq!(cc.max_age, Some(300.0));
        assert!(cc.public);
        assert!(!cc.no_cache);
    }

    #[test]
    fn test_cache_control_no_store() {
        let cc = CacheControl::parse("no-store, no-cache");
        assert!(cc.no_store);
        assert!(cc.no_cache);
        assert!(cc.is_uncacheable());
    }

    #[test]
    fn test_cache_control_s_maxage() {
        let cc = CacheControl::parse("s-maxage=600, max-age=300");
        assert_eq!(cc.s_maxage, Some(600.0));
        assert_eq!(cc.max_age, Some(300.0));
    }

    #[test]
    fn test_parse_rfc1123() {
        let date = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT");
        assert!(date.is_some());
    }

    #[test]
    fn test_parse_rfc850() {
        let date = parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT");
        assert!(date.is_some());
    }

    #[test]
    fn test_parse_asctime() {
        let date = parse_http_date("Sun Nov  6 08:49:37 1994");
        assert!(date.is_some());
    }

    #[test]
    fn test_etag_match() {
        assert!(etag_matches("\"abc\"", "\"abc\""));
        assert!(etag_matches("\"abc\", \"def\"", "\"def\""));
        assert!(etag_matches("W/\"abc\"", "W/\"abc\""));
        assert!(etag_matches("W/\"abc\"", "\"abc\""));
        assert!(!etag_matches("\"abc\"", "\"xyz\""));
    }

    #[test]
    fn test_ttl_with_max_age() {
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        resp.set_header("Cache-Control", "max-age=300");

        let now = VtimReal::from_secs(1000000.0);
        let calc = TtlCalculation::from_response(
            &resp,
            now,
            VtimDur::from_secs(120.0),
            VtimDur::from_secs(10.0),
            VtimDur::ZERO,
        );

        assert_eq!(calc.ttl.as_secs(), 300.0);
        assert!(!calc.uncacheable);
    }

    #[test]
    fn test_ttl_no_store() {
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        resp.set_header("Cache-Control", "no-store");

        let now = VtimReal::from_secs(1000000.0);
        let calc = TtlCalculation::from_response(
            &resp,
            now,
            VtimDur::from_secs(120.0),
            VtimDur::from_secs(10.0),
            VtimDur::ZERO,
        );

        assert!(calc.uncacheable);
    }

    #[test]
    fn test_ttl_s_maxage_priority() {
        let mut resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        resp.set_header("Cache-Control", "s-maxage=600, max-age=300");

        let now = VtimReal::from_secs(1000000.0);
        let calc = TtlCalculation::from_response(
            &resp,
            now,
            VtimDur::from_secs(120.0),
            VtimDur::from_secs(10.0),
            VtimDur::ZERO,
        );

        // s-maxage takes priority
        assert_eq!(calc.ttl.as_secs(), 600.0);
    }
}
