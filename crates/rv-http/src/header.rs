use rv_types::KnownHeader;

/// A single HTTP header: name + value.
#[derive(Debug, Clone)]
pub struct Header {
    pub name: String,
    pub value: String,
    pub known: Option<KnownHeader>,
}

impl Header {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        let known = KnownHeader::from_name(&name);
        Self {
            name,
            value: value.into(),
            known,
        }
    }

    pub fn name_eq_ignore_case(&self, other: &str) -> bool {
        self.name.eq_ignore_ascii_case(other)
    }
}

/// Collection of HTTP headers with Varnish-style operations.
/// Mirrors the behavior of cache_http.c header operations.
#[derive(Debug, Clone, Default)]
pub struct HeaderMap {
    headers: Vec<Header>,
}

impl HeaderMap {
    pub fn new() -> Self {
        Self {
            headers: Vec::new(),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            headers: Vec::with_capacity(capacity),
        }
    }

    /// Get the first header value matching the given name (case-insensitive).
    /// Equivalent to http_GetHdr in cache_http.c
    pub fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.name_eq_ignore_case(name))
            .map(|h| h.value.as_str())
    }

    /// Get all header values matching the given name.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|h| h.name_eq_ignore_case(name))
            .map(|h| h.value.as_str())
            .collect()
    }

    /// Set a header, replacing any existing header with the same name.
    /// Equivalent to http_SetHeader in cache_http.c
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        // Remove existing headers with same name
        self.headers.retain(|h| !h.name_eq_ignore_case(&name));
        self.headers.push(Header::new(name, value));
    }

    /// Append a header (does not remove existing headers with same name).
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.headers.push(Header::new(name, value));
    }

    /// Remove all headers matching the given name.
    /// Equivalent to http_Unset in cache_http.c
    pub fn unset(&mut self, name: &str) {
        self.headers.retain(|h| !h.name_eq_ignore_case(name));
    }

    /// Check if a header exists.
    pub fn contains(&self, name: &str) -> bool {
        self.headers.iter().any(|h| h.name_eq_ignore_case(name))
    }

    /// Get the number of headers.
    pub fn len(&self) -> usize {
        self.headers.len()
    }

    /// Check if there are no headers.
    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// Iterate over all headers.
    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.headers.iter()
    }

    /// Copy headers from another HeaderMap, optionally filtering by flags.
    /// Equivalent to http_FilterReq / http_FilterResp in cache_http.c
    pub fn copy_from_filtered(
        &mut self,
        source: &HeaderMap,
        filter: impl Fn(&Header) -> bool,
    ) {
        for h in source.headers.iter() {
            if filter(h) {
                self.headers.push(h.clone());
            }
        }
    }

    /// Filter out connection-specific headers (Connection, Keep-Alive, etc.)
    /// Equivalent to http_FilterFields in cache_http.c
    pub fn filter_connection_headers(&mut self) {
        // Get the Connection header value to find hop-by-hop headers
        let connection_headers: Vec<String> = self
            .get("Connection")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();

        self.headers.retain(|h| {
            let lower = h.name.to_ascii_lowercase();
            // Remove standard connection-specific headers
            if matches!(
                lower.as_str(),
                "connection"
                    | "keep-alive"
                    | "transfer-encoding"
                    | "upgrade"
                    | "http2-settings"
            ) {
                return false;
            }
            // Remove headers listed in Connection header
            if connection_headers.contains(&lower) {
                return false;
            }
            true
        });
    }

    /// Get Content-Length as u64.
    pub fn content_length(&self) -> Option<u64> {
        self.get("Content-Length")
            .and_then(|v| v.trim().parse::<u64>().ok())
    }

    /// Check if Transfer-Encoding: chunked is set.
    pub fn is_chunked(&self) -> bool {
        self.get("Transfer-Encoding")
            .map(|v| v.eq_ignore_ascii_case("chunked"))
            .unwrap_or(false)
    }

    /// Collect all headers into a serializable format.
    pub fn to_vec(&self) -> Vec<(String, String)> {
        self.headers
            .iter()
            .map(|h| (h.name.clone(), h.value.clone()))
            .collect()
    }

    /// Clear all headers.
    pub fn clear(&mut self) {
        self.headers.clear();
    }

    /// Total byte size of all headers (name + ": " + value + "\r\n").
    pub fn wire_size(&self) -> usize {
        self.headers
            .iter()
            .map(|h| h.name.len() + 2 + h.value.len() + 2)
            .sum()
    }
}

impl<'a> IntoIterator for &'a HeaderMap {
    type Item = &'a Header;
    type IntoIter = std::slice::Iter<'a, Header>;

    fn into_iter(self) -> Self::IntoIter {
        self.headers.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_set_get() {
        let mut headers = HeaderMap::new();
        headers.set("Content-Type", "text/html");
        assert_eq!(headers.get("content-type"), Some("text/html"));
        assert_eq!(headers.get("Content-Type"), Some("text/html"));
    }

    #[test]
    fn test_header_unset() {
        let mut headers = HeaderMap::new();
        headers.set("X-Custom", "value1");
        headers.append("X-Custom", "value2");
        assert_eq!(headers.get_all("X-Custom").len(), 2);
        headers.unset("x-custom");
        assert!(!headers.contains("X-Custom"));
    }

    #[test]
    fn test_content_length() {
        let mut headers = HeaderMap::new();
        headers.set("Content-Length", "42");
        assert_eq!(headers.content_length(), Some(42));
    }

    #[test]
    fn test_filter_connection_headers() {
        let mut headers = HeaderMap::new();
        headers.set("Host", "example.com");
        headers.set("Connection", "keep-alive, X-Custom");
        headers.set("Keep-Alive", "timeout=5");
        headers.set("X-Custom", "remove-me");
        headers.set("Content-Type", "text/html");

        headers.filter_connection_headers();

        assert!(headers.contains("Host"));
        assert!(headers.contains("Content-Type"));
        assert!(!headers.contains("Connection"));
        assert!(!headers.contains("Keep-Alive"));
        assert!(!headers.contains("X-Custom"));
    }
}
