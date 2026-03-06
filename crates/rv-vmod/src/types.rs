pub use rv_types::VclValue;

use rv_types::VtimDur;

use crate::error::VmodError;

/// Extension trait providing fallible conversions on VclValue that return
/// `Result<_, VmodError>`. The base VclValue in rv-types has infallible
/// conversions (returning defaults on failure). VMODs need strict validation,
/// so these methods produce proper errors.
pub trait VclValueExt {
    /// Attempt to convert this value to an i64.
    fn try_to_int(&self) -> Result<i64, VmodError>;
    /// Attempt to convert this value to an f64.
    fn try_to_real(&self) -> Result<f64, VmodError>;
    /// Attempt to convert this value to a bool.
    fn try_to_bool(&self) -> Result<bool, VmodError>;
    /// Attempt to convert this value to a VtimDur.
    fn try_to_duration(&self) -> Result<VtimDur, VmodError>;
}

impl VclValueExt for VclValue {
    fn try_to_int(&self) -> Result<i64, VmodError> {
        match self {
            VclValue::Int(i) => Ok(*i),
            VclValue::Real(r) => Ok(*r as i64),
            VclValue::Bool(b) => Ok(if *b { 1 } else { 0 }),
            VclValue::String(s) => s
                .trim()
                .parse::<i64>()
                .map_err(|e| VmodError::TypeMismatch(format!("cannot convert string to int: {e}"))),
            VclValue::Duration(d) => Ok(*d as i64),
            other => Err(VmodError::TypeMismatch(format!(
                "cannot convert {other} to int"
            ))),
        }
    }

    fn try_to_real(&self) -> Result<f64, VmodError> {
        match self {
            VclValue::Real(r) => Ok(*r),
            VclValue::Int(i) => Ok(*i as f64),
            VclValue::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            VclValue::String(s) => s.trim().parse::<f64>().map_err(|e| {
                VmodError::TypeMismatch(format!("cannot convert string to real: {e}"))
            }),
            VclValue::Duration(d) => Ok(*d),
            other => Err(VmodError::TypeMismatch(format!(
                "cannot convert {other} to real"
            ))),
        }
    }

    fn try_to_bool(&self) -> Result<bool, VmodError> {
        match self {
            VclValue::Bool(b) => Ok(*b),
            VclValue::Int(i) => Ok(*i != 0),
            VclValue::Real(r) => Ok(*r != 0.0),
            VclValue::String(s) => Ok(!s.is_empty()),
            VclValue::Duration(d) => Ok(*d > 0.0),
            VclValue::Void => Ok(false),
            other => Err(VmodError::TypeMismatch(format!(
                "cannot convert {other} to bool"
            ))),
        }
    }

    fn try_to_duration(&self) -> Result<VtimDur, VmodError> {
        match self {
            VclValue::Duration(d) => Ok(VtimDur::from_secs(*d)),
            VclValue::Real(r) => Ok(VtimDur::from_secs(*r)),
            VclValue::Int(i) => Ok(VtimDur::from_secs(*i as f64)),
            VclValue::String(s) => parse_duration_string(s),
            other => Err(VmodError::TypeMismatch(format!(
                "cannot convert {other} to duration"
            ))),
        }
    }
}

/// Parse a VCL-style duration string with error reporting.
fn parse_duration_string(s: &str) -> Result<VtimDur, VmodError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(VmodError::InvalidArgument(
            "empty duration string".to_string(),
        ));
    }

    if let Some(num) = s.strip_suffix("ms") {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|e| VmodError::InvalidArgument(format!("invalid duration: {e}")))?;
        return Ok(VtimDur::from_millis(val));
    }
    if let Some(num) = s.strip_suffix("min") {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|e| VmodError::InvalidArgument(format!("invalid duration: {e}")))?;
        return Ok(VtimDur::from_secs(val * 60.0));
    }
    if let Some(num) = s.strip_suffix('s') {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|e| VmodError::InvalidArgument(format!("invalid duration: {e}")))?;
        return Ok(VtimDur::from_secs(val));
    }
    if let Some(num) = s.strip_suffix('m') {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|e| VmodError::InvalidArgument(format!("invalid duration: {e}")))?;
        return Ok(VtimDur::from_secs(val * 60.0));
    }
    if let Some(num) = s.strip_suffix('h') {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|e| VmodError::InvalidArgument(format!("invalid duration: {e}")))?;
        return Ok(VtimDur::from_secs(val * 3600.0));
    }
    if let Some(num) = s.strip_suffix('d') {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|e| VmodError::InvalidArgument(format!("invalid duration: {e}")))?;
        return Ok(VtimDur::from_secs(val * 86400.0));
    }

    let val: f64 = s
        .parse()
        .map_err(|e| VmodError::InvalidArgument(format!("invalid duration: {e}")))?;
    Ok(VtimDur::from_secs(val))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_string_conversion() {
        assert_eq!(VclValue::String("hello".into()).to_string_value(), "hello");
        assert_eq!(VclValue::Int(42).to_string_value(), "42");
        assert_eq!(VclValue::Real(3.15).to_string_value(), "3.15");
        assert_eq!(VclValue::Bool(true).to_string_value(), "true");
        assert_eq!(VclValue::Bool(false).to_string_value(), "false");
        assert_eq!(
            VclValue::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)).to_string_value(),
            "127.0.0.1"
        );
        assert_eq!(VclValue::Blob(vec![0xde, 0xad]).to_string_value(), "dead");
        assert_eq!(VclValue::Void.to_string_value(), "");
    }

    #[test]
    fn test_try_to_int() {
        assert_eq!(VclValue::Int(100).try_to_int().unwrap(), 100);
        assert_eq!(VclValue::Real(3.7).try_to_int().unwrap(), 3);
        assert_eq!(VclValue::Bool(true).try_to_int().unwrap(), 1);
        assert_eq!(VclValue::Bool(false).try_to_int().unwrap(), 0);
        assert_eq!(VclValue::String("42".to_string()).try_to_int().unwrap(), 42);
        assert_eq!(VclValue::Duration(5.0).try_to_int().unwrap(), 5);
        assert!(
            VclValue::String("not_a_number".to_string())
                .try_to_int()
                .is_err()
        );
        assert!(VclValue::Blob(vec![1, 2]).try_to_int().is_err());
    }

    #[test]
    fn test_try_to_real() {
        assert_eq!(VclValue::Real(2.5).try_to_real().unwrap(), 2.5);
        assert_eq!(VclValue::Int(10).try_to_real().unwrap(), 10.0);
        assert_eq!(VclValue::Bool(true).try_to_real().unwrap(), 1.0);
        assert_eq!(
            VclValue::String("3.15".to_string()).try_to_real().unwrap(),
            3.15
        );
        assert_eq!(VclValue::Duration(2.5).try_to_real().unwrap(), 2.5);
        assert!(
            VclValue::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST))
                .try_to_real()
                .is_err()
        );
    }

    #[test]
    fn test_try_to_bool() {
        assert!(VclValue::Bool(true).try_to_bool().unwrap());
        assert!(!VclValue::Bool(false).try_to_bool().unwrap());
        assert!(VclValue::Int(1).try_to_bool().unwrap());
        assert!(!VclValue::Int(0).try_to_bool().unwrap());
        assert!(VclValue::Real(0.1).try_to_bool().unwrap());
        assert!(!VclValue::Real(0.0).try_to_bool().unwrap());
        assert!(VclValue::String("x".to_string()).try_to_bool().unwrap());
        assert!(!VclValue::String("".to_string()).try_to_bool().unwrap());
        assert!(VclValue::Duration(1.0).try_to_bool().unwrap());
        assert!(!VclValue::Void.try_to_bool().unwrap());
    }

    #[test]
    fn test_try_to_duration() {
        let d = VclValue::Duration(10.0).try_to_duration().unwrap();
        assert_eq!(d.as_secs(), 10.0);

        let d = VclValue::Real(5.0).try_to_duration().unwrap();
        assert_eq!(d.as_secs(), 5.0);

        let d = VclValue::Int(3).try_to_duration().unwrap();
        assert_eq!(d.as_secs(), 3.0);
    }

    #[test]
    fn test_parse_duration_via_ext() {
        let d = VclValue::String("5s".to_string())
            .try_to_duration()
            .unwrap();
        assert_eq!(d.as_secs(), 5.0);

        let d = VclValue::String("2m".to_string())
            .try_to_duration()
            .unwrap();
        assert_eq!(d.as_secs(), 120.0);

        let d = VclValue::String("1h".to_string())
            .try_to_duration()
            .unwrap();
        assert_eq!(d.as_secs(), 3600.0);

        let d = VclValue::String("1d".to_string())
            .try_to_duration()
            .unwrap();
        assert_eq!(d.as_secs(), 86400.0);

        let d = VclValue::String("500ms".to_string())
            .try_to_duration()
            .unwrap();
        assert_eq!(d.as_secs(), 0.5);

        let d = VclValue::String("1.5min".to_string())
            .try_to_duration()
            .unwrap();
        assert_eq!(d.as_secs(), 90.0);

        // Bare number
        let d = VclValue::String("10".to_string())
            .try_to_duration()
            .unwrap();
        assert_eq!(d.as_secs(), 10.0);
    }

    #[test]
    fn test_display() {
        let v = VclValue::Int(42);
        assert_eq!(format!("{v}"), "Int(42)");

        let v = VclValue::Blob(vec![1, 2, 3]);
        assert_eq!(format!("{v}"), "Blob(3 bytes)");
    }
}
