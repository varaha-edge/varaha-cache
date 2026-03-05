//! vmod_std -- standard VMOD providing common utility functions.
//!
//! This is the Rust equivalent of Varnish's built-in `std` VMOD,
//! offering functions for type conversions, string operations,
//! time, and logging.

use rv_types::VtimReal;

use crate::error::VmodError;
use crate::registry::VmodFunction;
use crate::types::{VclValue, VclValueExt};

/// std.random(lo, hi) - Generate a random f64 in the range [lo, hi).
pub struct StdRandom;

impl VmodFunction for StdRandom {
    fn name(&self) -> &str {
        "random"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.len() != 2 {
            return Err(VmodError::InvalidArgument(
                "random(lo, hi) requires exactly 2 arguments".to_string(),
            ));
        }
        let lo = args[0].try_to_real()?;
        let hi = args[1].try_to_real()?;
        if lo >= hi {
            return Err(VmodError::InvalidArgument(format!(
                "random: lo ({lo}) must be less than hi ({hi})"
            )));
        }
        // Simple pseudo-random using system time nanos as seed.
        // This mirrors Varnish's VMOD std.random which uses drand48 --
        // not cryptographic, just a quick distribution.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let fraction = (nanos as f64) / (u32::MAX as f64);
        let value = lo + fraction * (hi - lo);
        Ok(VclValue::Real(value))
    }
}

/// std.round(r) - Round a real number to the nearest integer.
pub struct StdRound;

impl VmodFunction for StdRound {
    fn name(&self) -> &str {
        "round"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.len() != 1 {
            return Err(VmodError::InvalidArgument(
                "round(r) requires exactly 1 argument".to_string(),
            ));
        }
        let r = args[0].try_to_real()?;
        Ok(VclValue::Int(r.round() as i64))
    }
}

/// std.integer(s, fallback) - Parse a string as an integer.
pub struct StdInteger;

impl VmodFunction for StdInteger {
    fn name(&self) -> &str {
        "integer"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.is_empty() || args.len() > 2 {
            return Err(VmodError::InvalidArgument(
                "integer(s [, fallback]) requires 1 or 2 arguments".to_string(),
            ));
        }
        match args[0].try_to_int() {
            Ok(val) => Ok(VclValue::Int(val)),
            Err(_) if args.len() == 2 => {
                let fallback = args[1].try_to_int()?;
                Ok(VclValue::Int(fallback))
            }
            Err(e) => Err(e),
        }
    }
}

/// std.real(s, fallback) - Parse a string as a real number.
pub struct StdReal;

impl VmodFunction for StdReal {
    fn name(&self) -> &str {
        "real"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.is_empty() || args.len() > 2 {
            return Err(VmodError::InvalidArgument(
                "real(s [, fallback]) requires 1 or 2 arguments".to_string(),
            ));
        }
        match args[0].try_to_real() {
            Ok(val) => Ok(VclValue::Real(val)),
            Err(_) if args.len() == 2 => {
                let fallback = args[1].try_to_real()?;
                Ok(VclValue::Real(fallback))
            }
            Err(e) => Err(e),
        }
    }
}

/// std.duration(s, fallback) - Parse a string as a duration.
pub struct StdDuration;

impl VmodFunction for StdDuration {
    fn name(&self) -> &str {
        "duration"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.is_empty() || args.len() > 2 {
            return Err(VmodError::InvalidArgument(
                "duration(s [, fallback]) requires 1 or 2 arguments".to_string(),
            ));
        }
        match args[0].try_to_duration() {
            Ok(dur) => Ok(VclValue::Duration(dur.as_secs())),
            Err(_) if args.len() == 2 => {
                let fallback = args[1].try_to_duration()?;
                Ok(VclValue::Duration(fallback.as_secs()))
            }
            Err(e) => Err(e),
        }
    }
}

/// std.log(msg) - Log a message via tracing at info level.
pub struct StdLog;

impl VmodFunction for StdLog {
    fn name(&self) -> &str {
        "log"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.len() != 1 {
            return Err(VmodError::InvalidArgument(
                "log(msg) requires exactly 1 argument".to_string(),
            ));
        }
        let msg = args[0].to_string_value();
        tracing::info!(target: "vmod_std", "{}", msg);
        Ok(VclValue::Void)
    }
}

/// std.strstr(s1, s2) - Check if s2 is a substring of s1.
///
/// Returns true (as Bool) if s2 is found within s1, false otherwise.
pub struct StdStrstr;

impl VmodFunction for StdStrstr {
    fn name(&self) -> &str {
        "strstr"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.len() != 2 {
            return Err(VmodError::InvalidArgument(
                "strstr(s1, s2) requires exactly 2 arguments".to_string(),
            ));
        }
        let s1 = args[0].to_string_value();
        let s2 = args[1].to_string_value();
        Ok(VclValue::Bool(s1.contains(&s2)))
    }
}

/// std.querysort(url) - Sort query string parameters alphabetically.
///
/// Parses the URL, extracts query parameters, sorts them by key,
/// and reconstructs the URL. This is useful for cache normalization.
pub struct StdQuerysort;

impl VmodFunction for StdQuerysort {
    fn name(&self) -> &str {
        "querysort"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if args.len() != 1 {
            return Err(VmodError::InvalidArgument(
                "querysort(url) requires exactly 1 argument".to_string(),
            ));
        }
        let url_str = args[0].to_string_value();
        let sorted = sort_query_string(&url_str);
        Ok(VclValue::String(sorted))
    }
}

/// Sort query string parameters in a URL by their key names.
fn sort_query_string(url_str: &str) -> String {
    // Split at the '?' to separate path from query
    let Some((base, query)) = url_str.split_once('?') else {
        // No query string, return as-is
        return url_str.to_string();
    };

    if query.is_empty() {
        return url_str.to_string();
    }

    // Split query into (fragment-free query, optional fragment)
    let (query_part, fragment) = match query.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (query, None),
    };

    // Collect and sort parameters
    let mut params: Vec<&str> = query_part.split('&').collect();
    params.sort_unstable();

    let mut result = format!("{base}?{}", params.join("&"));
    if let Some(frag) = fragment {
        result.push('#');
        result.push_str(frag);
    }
    result
}

/// std.now() - Return the current real time as a duration since the Unix epoch.
pub struct StdNow;

impl VmodFunction for StdNow {
    fn name(&self) -> &str {
        "now"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if !args.is_empty() {
            return Err(VmodError::InvalidArgument(
                "now() takes no arguments".to_string(),
            ));
        }
        let now = VtimReal::now();
        Ok(VclValue::Real(now.as_secs()))
    }
}

/// Build the full set of std module functions.
pub fn std_module() -> Vec<Box<dyn VmodFunction>> {
    vec![
        Box::new(StdRandom),
        Box::new(StdRound),
        Box::new(StdInteger),
        Box::new(StdReal),
        Box::new(StdDuration),
        Box::new(StdLog),
        Box::new(StdStrstr),
        Box::new(StdQuerysort),
        Box::new(StdNow),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round() {
        let f = StdRound;
        let result = f.call(&[VclValue::Real(3.7)]).unwrap();
        assert_eq!(result.to_int(), 4);

        let result = f.call(&[VclValue::Real(3.2)]).unwrap();
        assert_eq!(result.to_int(), 3);

        let result = f.call(&[VclValue::Real(-1.5)]).unwrap();
        assert_eq!(result.to_int(), -2);
    }

    #[test]
    fn test_integer_parse() {
        let f = StdInteger;
        let result = f.call(&[VclValue::String("42".to_string())]).unwrap();
        assert_eq!(result.to_int(), 42);

        // With fallback
        let result = f
            .call(&[
                VclValue::String("not_a_number".to_string()),
                VclValue::Int(0),
            ])
            .unwrap();
        assert_eq!(result.to_int(), 0);

        // Without fallback, error
        let result = f.call(&[VclValue::String("bad".to_string())]);
        assert!(result.is_err());
    }

    #[test]
    fn test_real_parse() {
        let f = StdReal;
        let result = f.call(&[VclValue::String("3.14".to_string())]).unwrap();
        assert_eq!(result.to_real(), 3.14);

        // Fallback
        let result = f
            .call(&[VclValue::String("bad".to_string()), VclValue::Real(0.0)])
            .unwrap();
        assert_eq!(result.to_real(), 0.0);
    }

    #[test]
    fn test_duration_parse() {
        let f = StdDuration;
        let result = f.call(&[VclValue::String("5s".to_string())]).unwrap();
        if let VclValue::Duration(d) = result {
            assert_eq!(d, 5.0);
        } else {
            panic!("expected Duration");
        }

        // Fallback
        let result = f
            .call(&[
                VclValue::String("bad".to_string()),
                VclValue::Duration(10.0),
            ])
            .unwrap();
        if let VclValue::Duration(d) = result {
            assert_eq!(d, 10.0);
        } else {
            panic!("expected Duration");
        }
    }

    #[test]
    fn test_log() {
        let f = StdLog;
        let result = f
            .call(&[VclValue::String("test message".to_string())])
            .unwrap();
        assert!(matches!(result, VclValue::Void));
    }

    #[test]
    fn test_strstr() {
        let f = StdStrstr;
        let result = f
            .call(&[
                VclValue::String("hello world".to_string()),
                VclValue::String("world".to_string()),
            ])
            .unwrap();
        assert!(result.to_bool());

        let result = f
            .call(&[
                VclValue::String("hello world".to_string()),
                VclValue::String("xyz".to_string()),
            ])
            .unwrap();
        assert!(!result.to_bool());
    }

    #[test]
    fn test_querysort() {
        let f = StdQuerysort;

        // Sort query params
        let result = f
            .call(&[VclValue::String("/path?c=3&a=1&b=2".to_string())])
            .unwrap();
        assert_eq!(result.to_string_value(), "/path?a=1&b=2&c=3");

        // No query string
        let result = f.call(&[VclValue::String("/path".to_string())]).unwrap();
        assert_eq!(result.to_string_value(), "/path");

        // Empty query string
        let result = f.call(&[VclValue::String("/path?".to_string())]).unwrap();
        assert_eq!(result.to_string_value(), "/path?");

        // Preserve fragment
        let result = f
            .call(&[VclValue::String("/path?z=1&a=2#section".to_string())])
            .unwrap();
        assert_eq!(result.to_string_value(), "/path?a=2&z=1#section");
    }

    #[test]
    fn test_now() {
        let f = StdNow;
        let result = f.call(&[]).unwrap();
        let ts = result.to_real();
        // Should be a reasonable Unix timestamp (after 2020)
        assert!(ts > 1_577_836_800.0);
    }

    #[test]
    fn test_random_range() {
        let f = StdRandom;
        let result = f.call(&[VclValue::Real(0.0), VclValue::Real(1.0)]).unwrap();
        let val = result.to_real();
        assert!(val >= 0.0 && val < 1.0, "random value {val} out of range");
    }

    #[test]
    fn test_random_invalid_range() {
        let f = StdRandom;
        let result = f.call(&[VclValue::Real(5.0), VclValue::Real(1.0)]);
        assert!(result.is_err());
    }

    #[test]
    fn test_round_wrong_args() {
        let f = StdRound;
        assert!(f.call(&[]).is_err());
        assert!(f.call(&[VclValue::Real(1.0), VclValue::Real(2.0)]).is_err());
    }

    #[test]
    fn test_std_module_factory() {
        let funcs = std_module();
        let names: Vec<&str> = funcs.iter().map(|f| f.name()).collect();
        assert!(names.contains(&"random"));
        assert!(names.contains(&"round"));
        assert!(names.contains(&"integer"));
        assert!(names.contains(&"real"));
        assert!(names.contains(&"duration"));
        assert!(names.contains(&"log"));
        assert!(names.contains(&"strstr"));
        assert!(names.contains(&"querysort"));
        assert!(names.contains(&"now"));
    }
}
