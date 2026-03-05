use crate::header::HeaderMap;

/// Vary matching for cached responses.
/// Based on cache_vary.c logic.
///
/// When a response has a Vary header, we need to match subsequent requests
/// against the stored vary data to determine if the cached response applies.
pub struct VaryMatcher;

impl VaryMatcher {
    /// Build vary matching data from a response's Vary header and the original request.
    /// This produces a blob that can be stored with the cached object and used
    /// for future match comparisons.
    ///
    /// Returns None if there's no Vary header (object matches all requests).
    pub fn build_vary_data(
        response_vary: Option<&str>,
        request_headers: &HeaderMap,
    ) -> Option<Vec<u8>> {
        let vary = response_vary?;
        if vary.trim() == "*" {
            // Vary: * means never match - return a special marker
            return Some(vec![0xFF]);
        }

        let mut data = Vec::new();

        for field_name in vary.split(',') {
            let field_name = field_name.trim();
            if field_name.is_empty() {
                continue;
            }

            let field_value = request_headers.get(field_name).unwrap_or("");

            // Store: length of name, name, length of value, value
            let name_bytes = field_name.as_bytes();
            let value_bytes = field_value.as_bytes();

            // Store name length as u16
            data.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            data.extend_from_slice(name_bytes);
            // Store value length as u16
            data.extend_from_slice(&(value_bytes.len() as u16).to_le_bytes());
            data.extend_from_slice(value_bytes);
        }

        if data.is_empty() {
            None
        } else {
            Some(data)
        }
    }

    /// Check if a new request matches the stored vary data.
    /// Returns true if the request matches (cached object can be served).
    pub fn matches(vary_data: &[u8], request_headers: &HeaderMap) -> bool {
        // Special case: Vary: * never matches
        if vary_data == [0xFF] {
            return false;
        }

        let mut pos = 0;

        while pos < vary_data.len() {
            // Read name length
            if pos + 2 > vary_data.len() {
                return false;
            }
            let name_len =
                u16::from_le_bytes([vary_data[pos], vary_data[pos + 1]]) as usize;
            pos += 2;

            // Read name
            if pos + name_len > vary_data.len() {
                return false;
            }
            let name = match std::str::from_utf8(&vary_data[pos..pos + name_len]) {
                Ok(n) => n,
                Err(_) => return false,
            };
            pos += name_len;

            // Read value length
            if pos + 2 > vary_data.len() {
                return false;
            }
            let value_len =
                u16::from_le_bytes([vary_data[pos], vary_data[pos + 1]]) as usize;
            pos += 2;

            // Read stored value
            if pos + value_len > vary_data.len() {
                return false;
            }
            let stored_value = match std::str::from_utf8(&vary_data[pos..pos + value_len]) {
                Ok(v) => v,
                Err(_) => return false,
            };
            pos += value_len;

            // Compare with request header
            let request_value = request_headers.get(name).unwrap_or("");
            if request_value != stored_value {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_vary() {
        let req = HeaderMap::new();
        assert!(VaryMatcher::build_vary_data(None, &req).is_none());
    }

    #[test]
    fn test_vary_star() {
        let req = HeaderMap::new();
        let data = VaryMatcher::build_vary_data(Some("*"), &req).unwrap();
        assert_eq!(data, vec![0xFF]);
        assert!(!VaryMatcher::matches(&data, &req));
    }

    #[test]
    fn test_vary_match() {
        let mut req1 = HeaderMap::new();
        req1.set("Accept-Encoding", "gzip");

        let data =
            VaryMatcher::build_vary_data(Some("Accept-Encoding"), &req1).unwrap();

        // Same request matches
        let mut req2 = HeaderMap::new();
        req2.set("Accept-Encoding", "gzip");
        assert!(VaryMatcher::matches(&data, &req2));

        // Different value doesn't match
        let mut req3 = HeaderMap::new();
        req3.set("Accept-Encoding", "br");
        assert!(!VaryMatcher::matches(&data, &req3));
    }

    #[test]
    fn test_vary_multiple_headers() {
        let mut req1 = HeaderMap::new();
        req1.set("Accept-Encoding", "gzip");
        req1.set("Accept-Language", "en");

        let data =
            VaryMatcher::build_vary_data(Some("Accept-Encoding, Accept-Language"), &req1)
                .unwrap();

        let mut req2 = HeaderMap::new();
        req2.set("Accept-Encoding", "gzip");
        req2.set("Accept-Language", "en");
        assert!(VaryMatcher::matches(&data, &req2));

        let mut req3 = HeaderMap::new();
        req3.set("Accept-Encoding", "gzip");
        req3.set("Accept-Language", "fr");
        assert!(!VaryMatcher::matches(&data, &req3));
    }
}
