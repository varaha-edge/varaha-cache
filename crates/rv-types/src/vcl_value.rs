use std::fmt;
use std::net::IpAddr;

/// Unified VCL runtime value type.
///
/// Represents all value types in the VCL type system: strings, integers,
/// reals, durations, booleans, IP addresses, binary blobs, and void.
/// Duration is stored as f64 seconds because the interpreter performs
/// all arithmetic on f64.
#[derive(Debug, Clone)]
pub enum VclValue {
    /// A string value.
    String(std::string::String),
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit floating-point number.
    Real(f64),
    /// A duration in seconds (f64).
    Duration(f64),
    /// A boolean value.
    Bool(bool),
    /// An IP address.
    Ip(IpAddr),
    /// A binary blob.
    Blob(Vec<u8>),
    /// Void (no value).
    Void,
}

impl VclValue {
    /// Convert this value to a string representation.
    pub fn to_string_value(&self) -> std::string::String {
        match self {
            VclValue::String(s) => s.clone(),
            VclValue::Int(i) => i.to_string(),
            VclValue::Real(r) => format!("{r}"),
            VclValue::Duration(d) => format!("{d}s"),
            VclValue::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            VclValue::Ip(ip) => ip.to_string(),
            VclValue::Blob(data) => {
                let mut hex = std::string::String::with_capacity(data.len() * 2);
                for byte in data {
                    hex.push_str(&format!("{byte:02x}"));
                }
                hex
            }
            VclValue::Void => std::string::String::new(),
        }
    }

    /// Convert this value to a bool (infallible).
    pub fn to_bool(&self) -> bool {
        match self {
            VclValue::Bool(b) => *b,
            VclValue::Int(i) => *i != 0,
            VclValue::Real(r) => *r != 0.0,
            VclValue::String(s) => !s.is_empty(),
            VclValue::Duration(d) => *d > 0.0,
            VclValue::Void => false,
            VclValue::Ip(_) => true,
            VclValue::Blob(data) => !data.is_empty(),
        }
    }

    /// Convert this value to an f64 (infallible).
    pub fn to_real(&self) -> f64 {
        match self {
            VclValue::Real(r) => *r,
            VclValue::Int(i) => *i as f64,
            VclValue::Duration(d) => *d,
            VclValue::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            VclValue::String(s) => s.parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// Convert this value to an i64 (infallible).
    pub fn to_int(&self) -> i64 {
        match self {
            VclValue::Int(i) => *i,
            VclValue::Real(r) => *r as i64,
            VclValue::Duration(d) => *d as i64,
            VclValue::Bool(b) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            VclValue::String(s) => s.parse().unwrap_or(0),
            _ => 0,
        }
    }

    /// Convert this value to a duration in seconds (infallible).
    pub fn to_duration_secs(&self) -> f64 {
        match self {
            VclValue::Duration(d) => *d,
            VclValue::Real(r) => *r,
            VclValue::Int(i) => *i as f64,
            VclValue::String(s) => parse_duration_str(s),
            _ => 0.0,
        }
    }
}

impl fmt::Display for VclValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VclValue::String(s) => write!(f, "String({s})"),
            VclValue::Int(i) => write!(f, "Int({i})"),
            VclValue::Real(r) => write!(f, "Real({r})"),
            VclValue::Duration(d) => write!(f, "Duration({d}s)"),
            VclValue::Bool(b) => write!(f, "Bool({b})"),
            VclValue::Ip(ip) => write!(f, "Ip({ip})"),
            VclValue::Blob(data) => write!(f, "Blob({} bytes)", data.len()),
            VclValue::Void => write!(f, "Void"),
        }
    }
}

/// Parse a VCL-style duration string into seconds.
///
/// Supported formats: "Ns" (seconds), "Nm" or "Nmin" (minutes),
/// "Nh" (hours), "Nd" (days), "Nms" (milliseconds), or a bare
/// number (interpreted as seconds).
pub fn parse_duration_str(s: &str) -> f64 {
    let s = s.trim();
    if let Some(num) = s.strip_suffix("ms") {
        return num.trim().parse::<f64>().unwrap_or(0.0) / 1000.0;
    }
    if let Some(num) = s.strip_suffix("min") {
        return num.trim().parse::<f64>().unwrap_or(0.0) * 60.0;
    }
    if let Some(num) = s.strip_suffix('s') {
        return num.trim().parse::<f64>().unwrap_or(0.0);
    }
    if let Some(num) = s.strip_suffix('m') {
        return num.trim().parse::<f64>().unwrap_or(0.0) * 60.0;
    }
    if let Some(num) = s.strip_suffix('h') {
        return num.trim().parse::<f64>().unwrap_or(0.0) * 3600.0;
    }
    if let Some(num) = s.strip_suffix('d') {
        return num.trim().parse::<f64>().unwrap_or(0.0) * 86400.0;
    }
    s.parse::<f64>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_string_conversion() {
        assert_eq!(VclValue::String("hello".into()).to_string_value(), "hello");
        assert_eq!(VclValue::Int(42).to_string_value(), "42");
        assert_eq!(VclValue::Real(3.14).to_string_value(), "3.14");
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
    fn test_to_bool() {
        assert!(VclValue::Bool(true).to_bool());
        assert!(!VclValue::Bool(false).to_bool());
        assert!(VclValue::Int(1).to_bool());
        assert!(!VclValue::Int(0).to_bool());
        assert!(VclValue::Real(0.1).to_bool());
        assert!(!VclValue::Real(0.0).to_bool());
        assert!(VclValue::String("x".to_string()).to_bool());
        assert!(!VclValue::String("".to_string()).to_bool());
        assert!(VclValue::Duration(1.0).to_bool());
        assert!(!VclValue::Void.to_bool());
        assert!(VclValue::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)).to_bool());
        assert!(VclValue::Blob(vec![1]).to_bool());
        assert!(!VclValue::Blob(vec![]).to_bool());
    }

    #[test]
    fn test_to_real() {
        assert_eq!(VclValue::Real(2.5).to_real(), 2.5);
        assert_eq!(VclValue::Int(10).to_real(), 10.0);
        assert_eq!(VclValue::Duration(5.0).to_real(), 5.0);
        assert_eq!(VclValue::Bool(true).to_real(), 1.0);
        assert_eq!(VclValue::String("3.14".to_string()).to_real(), 3.14);
    }

    #[test]
    fn test_to_int() {
        assert_eq!(VclValue::Int(100).to_int(), 100);
        assert_eq!(VclValue::Real(3.7).to_int(), 3);
        assert_eq!(VclValue::Bool(true).to_int(), 1);
        assert_eq!(VclValue::Bool(false).to_int(), 0);
        assert_eq!(VclValue::String("42".to_string()).to_int(), 42);
        assert_eq!(VclValue::Duration(5.0).to_int(), 5);
    }

    #[test]
    fn test_to_duration_secs() {
        assert_eq!(VclValue::Duration(10.0).to_duration_secs(), 10.0);
        assert_eq!(VclValue::Real(5.0).to_duration_secs(), 5.0);
        assert_eq!(VclValue::Int(3).to_duration_secs(), 3.0);
        assert_eq!(VclValue::String("5s".to_string()).to_duration_secs(), 5.0);
        assert_eq!(VclValue::String("2m".to_string()).to_duration_secs(), 120.0);
        assert_eq!(
            VclValue::String("1h".to_string()).to_duration_secs(),
            3600.0
        );
        assert_eq!(
            VclValue::String("1d".to_string()).to_duration_secs(),
            86400.0
        );
        assert_eq!(
            VclValue::String("500ms".to_string()).to_duration_secs(),
            0.5
        );
        assert_eq!(
            VclValue::String("1.5min".to_string()).to_duration_secs(),
            90.0
        );
        assert_eq!(VclValue::String("10".to_string()).to_duration_secs(), 10.0);
    }

    #[test]
    fn test_display() {
        let v = VclValue::Int(42);
        assert_eq!(format!("{v}"), "Int(42)");

        let v = VclValue::Blob(vec![1, 2, 3]);
        assert_eq!(format!("{v}"), "Blob(3 bytes)");
    }

    #[test]
    fn test_blob_conversions() {
        let blob = VclValue::Blob(vec![0xca, 0xfe]);
        assert_eq!(blob.to_string_value(), "cafe");
        assert!(blob.to_bool());
        assert_eq!(blob.to_int(), 0);
        assert_eq!(blob.to_real(), 0.0);

        let empty = VclValue::Blob(vec![]);
        assert!(!empty.to_bool());
    }
}
