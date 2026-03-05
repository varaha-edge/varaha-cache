use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("empty input string")]
    Empty,

    #[error("invalid number in \"{0}\": {1}")]
    InvalidNumber(String, String),

    #[error("unknown duration suffix in \"{0}\" (expected s, ms, m, h, or d)")]
    UnknownDurationSuffix(String),

    #[error("unknown size suffix in \"{0}\" (expected b, k, m, g, or t)")]
    UnknownSizeSuffix(String),

    #[error("size overflow computing \"{0}\"")]
    SizeOverflow(String),
}

/// Parse a human-readable duration string into a [`Duration`].
///
/// Supported suffixes (case-insensitive):
///  - `ms` -- milliseconds
///  - `s`  -- seconds
///  - `m`  -- minutes
///  - `h`  -- hours
///  - `d`  -- days
///
/// The numeric part may be an integer or a floating-point value.
///
/// # Examples
/// ```
/// use rv_config::parse::parse_duration;
/// use std::time::Duration;
///
/// assert_eq!(parse_duration("120s").unwrap(), Duration::from_secs(120));
/// assert_eq!(parse_duration("3.5s").unwrap(), Duration::from_secs_f64(3.5));
/// assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
/// assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
/// assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
/// assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
/// ```
pub fn parse_duration(input: &str) -> Result<Duration, ParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(ParseError::Empty);
    }

    let lower = s.to_ascii_lowercase();

    // Detect suffix -- check "ms" first (two-char) before "m" or "s" (one-char).
    let (num_part, multiplier) = if let Some(num) = lower.strip_suffix("ms") {
        (num, 0.001_f64)
    } else if let Some(num) = lower.strip_suffix('s') {
        (num, 1.0_f64)
    } else if let Some(num) = lower.strip_suffix('m') {
        (num, 60.0_f64)
    } else if let Some(num) = lower.strip_suffix('h') {
        (num, 3600.0_f64)
    } else if let Some(num) = lower.strip_suffix('d') {
        (num, 86400.0_f64)
    } else {
        return Err(ParseError::UnknownDurationSuffix(s.to_string()));
    };

    let value: f64 = num_part
        .parse()
        .map_err(|e: std::num::ParseFloatError| ParseError::InvalidNumber(s.to_string(), e.to_string()))?;

    Ok(Duration::from_secs_f64(value * multiplier))
}

/// Parse a human-readable size string into bytes.
///
/// Supported suffixes (case-insensitive):
///  - `b` or no suffix -- bytes
///  - `k` / `kb` -- kibibytes (1024)
///  - `m` / `mb` -- mebibytes (1024^2)
///  - `g` / `gb` -- gibibytes (1024^3)
///  - `t` / `tb` -- tebibytes (1024^4)
///
/// The numeric part must be a non-negative integer or float, but the
/// result is truncated to a whole number of bytes.
///
/// # Examples
/// ```
/// use rv_config::parse::parse_size;
///
/// assert_eq!(parse_size("256m").unwrap(), 256 * 1024 * 1024);
/// assert_eq!(parse_size("1g").unwrap(), 1024 * 1024 * 1024);
/// assert_eq!(parse_size("512K").unwrap(), 512 * 1024);
/// assert_eq!(parse_size("4096b").unwrap(), 4096);
/// assert_eq!(parse_size("1024").unwrap(), 1024);
/// ```
pub fn parse_size(input: &str) -> Result<usize, ParseError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(ParseError::Empty);
    }

    let lower = s.to_ascii_lowercase();

    // Detect suffix -- check two-char suffixes first.
    let (num_part, multiplier): (&str, u64) = if let Some(num) = lower.strip_suffix("tb") {
        (num, 1u64 << 40)
    } else if let Some(num) = lower.strip_suffix("gb") {
        (num, 1u64 << 30)
    } else if let Some(num) = lower.strip_suffix("mb") {
        (num, 1u64 << 20)
    } else if let Some(num) = lower.strip_suffix("kb") {
        (num, 1u64 << 10)
    } else if let Some(num) = lower.strip_suffix('t') {
        (num, 1u64 << 40)
    } else if let Some(num) = lower.strip_suffix('g') {
        (num, 1u64 << 30)
    } else if let Some(num) = lower.strip_suffix('m') {
        (num, 1u64 << 20)
    } else if let Some(num) = lower.strip_suffix('k') {
        (num, 1u64 << 10)
    } else if let Some(num) = lower.strip_suffix('b') {
        (num, 1)
    } else {
        // No suffix -- treat as bytes.
        (lower.as_str(), 1)
    };

    let value: f64 = num_part
        .parse()
        .map_err(|e: std::num::ParseFloatError| ParseError::InvalidNumber(s.to_string(), e.to_string()))?;

    let bytes = value * (multiplier as f64);
    if bytes < 0.0 || bytes > (usize::MAX as f64) {
        return Err(ParseError::SizeOverflow(s.to_string()));
    }

    Ok(bytes as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // parse_duration
    // -----------------------------------------------------------------------

    #[test]
    fn duration_seconds() {
        assert_eq!(parse_duration("120s").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn duration_fractional_seconds() {
        assert_eq!(parse_duration("3.5s").unwrap(), Duration::from_secs_f64(3.5));
    }

    #[test]
    fn duration_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn duration_minutes() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn duration_days() {
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
    }

    #[test]
    fn duration_case_insensitive() {
        assert_eq!(parse_duration("500MS").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("10S").unwrap(), Duration::from_secs(10));
    }

    #[test]
    fn duration_empty_err() {
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn duration_no_suffix_err() {
        assert!(parse_duration("42").is_err());
    }

    #[test]
    fn duration_bad_number_err() {
        assert!(parse_duration("abcs").is_err());
    }

    // -----------------------------------------------------------------------
    // parse_size
    // -----------------------------------------------------------------------

    #[test]
    fn size_bytes() {
        assert_eq!(parse_size("4096b").unwrap(), 4096);
    }

    #[test]
    fn size_no_suffix() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
    }

    #[test]
    fn size_kibibytes() {
        assert_eq!(parse_size("512k").unwrap(), 512 * 1024);
    }

    #[test]
    fn size_mebibytes() {
        assert_eq!(parse_size("256m").unwrap(), 256 * 1024 * 1024);
    }

    #[test]
    fn size_gibibytes() {
        assert_eq!(parse_size("1g").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn size_tebibytes() {
        assert_eq!(parse_size("1t").unwrap(), 1usize << 40);
    }

    #[test]
    fn size_case_insensitive() {
        assert_eq!(parse_size("256M").unwrap(), 256 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn size_with_b_suffix() {
        assert_eq!(parse_size("256MB").unwrap(), 256 * 1024 * 1024);
        assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn size_empty_err() {
        assert!(parse_size("").is_err());
    }

    #[test]
    fn size_bad_number_err() {
        assert!(parse_size("xyzm").is_err());
    }
}
