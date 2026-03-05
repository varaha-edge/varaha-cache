//! vmod_cookie -- HTTP cookie manipulation.
//!
//! Provides parsing, reading, modification, and filtering of HTTP cookies.
//! This mirrors the functionality of vmod_cookie in Varnish.

use std::collections::HashMap;

use regex::Regex;

/// A jar of cookies parsed from HTTP headers.
///
/// Provides methods for parsing Cookie/Set-Cookie headers and manipulating
/// the resulting name-value pairs.
#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    cookies: HashMap<String, String>,
}

impl CookieJar {
    /// Create a new empty cookie jar.
    pub fn new() -> Self {
        Self {
            cookies: HashMap::new(),
        }
    }

    /// Parse a Cookie header value (semicolon-separated name=value pairs).
    ///
    /// Example input: "name1=value1; name2=value2; name3=value3"
    pub fn parse(&mut self, header_value: &str) {
        for pair in header_value.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            if let Some((name, value)) = pair.split_once('=') {
                let name = name.trim().to_string();
                let value = value.trim().to_string();
                if !name.is_empty() {
                    self.cookies.insert(name, value);
                }
            }
        }
    }

    /// Get the value of a cookie by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(|s| s.as_str())
    }

    /// Set a cookie value, creating it if it does not exist.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.cookies.insert(name.into(), value.into());
    }

    /// Delete a cookie by name.
    ///
    /// Returns true if the cookie existed and was removed.
    pub fn delete(&mut self, name: &str) -> bool {
        self.cookies.remove(name).is_some()
    }

    /// Keep only cookies whose names match the given regex pattern.
    ///
    /// All cookies whose names do not match are removed from the jar.
    pub fn filter_re(&mut self, pattern: &str) -> Result<(), regex::Error> {
        let re = Regex::new(pattern)?;
        self.cookies.retain(|name, _| re.is_match(name));
        Ok(())
    }

    /// Return the number of cookies in the jar.
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    /// Check whether the jar is empty.
    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Format cookies back into a Cookie header value.
    pub fn to_header_value(&self) -> String {
        let mut pairs: Vec<String> = self
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        pairs.sort();
        pairs.join("; ")
    }

    /// Clear all cookies from the jar.
    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    /// Iterate over all cookies as (name, value) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.cookies.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cookie_header() {
        let mut jar = CookieJar::new();
        jar.parse("session=abc123; user=john; theme=dark");

        assert_eq!(jar.get("session"), Some("abc123"));
        assert_eq!(jar.get("user"), Some("john"));
        assert_eq!(jar.get("theme"), Some("dark"));
        assert_eq!(jar.len(), 3);
    }

    #[test]
    fn test_parse_with_whitespace() {
        let mut jar = CookieJar::new();
        jar.parse("  name = value ;  foo  =  bar  ");

        assert_eq!(jar.get("name"), Some("value"));
        assert_eq!(jar.get("foo"), Some("bar"));
    }

    #[test]
    fn test_parse_empty() {
        let mut jar = CookieJar::new();
        jar.parse("");
        assert!(jar.is_empty());
    }

    #[test]
    fn test_parse_single_cookie() {
        let mut jar = CookieJar::new();
        jar.parse("session=xyz");
        assert_eq!(jar.get("session"), Some("xyz"));
        assert_eq!(jar.len(), 1);
    }

    #[test]
    fn test_set_and_get() {
        let mut jar = CookieJar::new();
        jar.set("new_cookie", "new_value");
        assert_eq!(jar.get("new_cookie"), Some("new_value"));
    }

    #[test]
    fn test_set_overwrite() {
        let mut jar = CookieJar::new();
        jar.set("name", "old");
        jar.set("name", "new");
        assert_eq!(jar.get("name"), Some("new"));
        assert_eq!(jar.len(), 1);
    }

    #[test]
    fn test_delete() {
        let mut jar = CookieJar::new();
        jar.parse("a=1; b=2; c=3");

        assert!(jar.delete("b"));
        assert_eq!(jar.get("b"), None);
        assert_eq!(jar.len(), 2);

        // Deleting nonexistent cookie returns false
        assert!(!jar.delete("nonexistent"));
    }

    #[test]
    fn test_filter_re() {
        let mut jar = CookieJar::new();
        jar.parse("session_id=abc; user_id=123; theme=dark; lang=en");

        // Keep only cookies ending with "_id"
        jar.filter_re("_id$").unwrap();
        assert_eq!(jar.len(), 2);
        assert!(jar.get("session_id").is_some());
        assert!(jar.get("user_id").is_some());
        assert!(jar.get("theme").is_none());
        assert!(jar.get("lang").is_none());
    }

    #[test]
    fn test_filter_re_prefix() {
        let mut jar = CookieJar::new();
        jar.parse("__ga=x; __gid=y; session=z; user=w");

        // Keep only cookies starting with "__"
        jar.filter_re("^__").unwrap();
        assert_eq!(jar.len(), 2);
        assert!(jar.get("__ga").is_some());
        assert!(jar.get("__gid").is_some());
    }

    #[test]
    fn test_filter_re_invalid_pattern() {
        let mut jar = CookieJar::new();
        let result = jar.filter_re("[invalid");
        assert!(result.is_err());
    }

    #[test]
    fn test_to_header_value() {
        let mut jar = CookieJar::new();
        jar.set("b", "2");
        jar.set("a", "1");

        let header = jar.to_header_value();
        // Sorted alphabetically
        assert_eq!(header, "a=1; b=2");
    }

    #[test]
    fn test_clear() {
        let mut jar = CookieJar::new();
        jar.parse("a=1; b=2");
        assert_eq!(jar.len(), 2);

        jar.clear();
        assert!(jar.is_empty());
    }

    #[test]
    fn test_get_nonexistent() {
        let jar = CookieJar::new();
        assert_eq!(jar.get("nothing"), None);
    }

    #[test]
    fn test_parse_malformed_entries() {
        let mut jar = CookieJar::new();
        // Entries without '=' are silently skipped
        jar.parse("good=value; malformed; also_good=ok");
        assert_eq!(jar.len(), 2);
        assert_eq!(jar.get("good"), Some("value"));
        assert_eq!(jar.get("also_good"), Some("ok"));
    }

    #[test]
    fn test_parse_value_with_equals() {
        let mut jar = CookieJar::new();
        // Value contains '=' (e.g., base64)
        jar.parse("token=abc=def==");
        assert_eq!(jar.get("token"), Some("abc=def=="));
    }

    #[test]
    fn test_iter() {
        let mut jar = CookieJar::new();
        jar.set("x", "1");
        jar.set("y", "2");

        let mut pairs: Vec<(&str, &str)> = jar.iter().collect();
        pairs.sort();
        assert_eq!(pairs, vec![("x", "1"), ("y", "2")]);
    }
}
