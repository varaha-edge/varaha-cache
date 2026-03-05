use thiserror::Error;

#[derive(Debug, Error)]
pub enum RangeError {
    #[error("invalid range specification")]
    InvalidRange,
    #[error("range not satisfiable")]
    NotSatisfiable,
}

/// A single byte range specification.
/// Based on cache_range.c
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeSpec {
    /// Start byte position (inclusive). None means from the end.
    pub start: Option<u64>,
    /// End byte position (inclusive). None means to the end.
    pub end: Option<u64>,
}

impl RangeSpec {
    /// Resolve this range against a known content length.
    /// Returns (start, end) inclusive byte positions.
    pub fn resolve(&self, content_length: u64) -> Result<(u64, u64), RangeError> {
        match (self.start, self.end) {
            (Some(start), Some(end)) => {
                if start > end || start >= content_length {
                    return Err(RangeError::NotSatisfiable);
                }
                let end = end.min(content_length - 1);
                Ok((start, end))
            }
            (Some(start), None) => {
                if start >= content_length {
                    return Err(RangeError::NotSatisfiable);
                }
                Ok((start, content_length - 1))
            }
            (None, Some(suffix_length)) => {
                if suffix_length == 0 {
                    return Err(RangeError::NotSatisfiable);
                }
                let start = content_length.saturating_sub(suffix_length);
                Ok((start, content_length - 1))
            }
            (None, None) => Err(RangeError::InvalidRange),
        }
    }

    /// Length of this range after resolving against content length.
    pub fn resolved_length(&self, content_length: u64) -> Result<u64, RangeError> {
        let (start, end) = self.resolve(content_length)?;
        Ok(end - start + 1)
    }
}

/// A set of byte ranges from a Range header.
#[derive(Debug, Clone)]
pub struct RangeSet {
    pub ranges: Vec<RangeSpec>,
}

impl RangeSet {
    /// Parse a Range header value.
    /// Supports "bytes=start-end, start-end, ..." format.
    pub fn parse(header_value: &str) -> Result<Self, RangeError> {
        let header_value = header_value.trim();

        // Must start with "bytes="
        let ranges_str = header_value
            .strip_prefix("bytes=")
            .ok_or(RangeError::InvalidRange)?;

        let mut ranges = Vec::new();

        for part in ranges_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            let range = if let Some(suffix) = part.strip_prefix('-') {
                // Suffix range: -500 means last 500 bytes
                let suffix_length: u64 = suffix.parse().map_err(|_| RangeError::InvalidRange)?;
                RangeSpec {
                    start: None,
                    end: Some(suffix_length),
                }
            } else if let Some((start_str, end_str)) = part.split_once('-') {
                let start: u64 = start_str
                    .parse()
                    .map_err(|_| RangeError::InvalidRange)?;
                if end_str.is_empty() {
                    // Open-ended range: 500- means from 500 to end
                    RangeSpec {
                        start: Some(start),
                        end: None,
                    }
                } else {
                    let end: u64 = end_str
                        .parse()
                        .map_err(|_| RangeError::InvalidRange)?;
                    RangeSpec {
                        start: Some(start),
                        end: Some(end),
                    }
                }
            } else {
                return Err(RangeError::InvalidRange);
            };

            ranges.push(range);
        }

        if ranges.is_empty() {
            return Err(RangeError::InvalidRange);
        }

        Ok(Self { ranges })
    }

    /// Check if this is a single range request.
    pub fn is_single(&self) -> bool {
        self.ranges.len() == 1
    }

    /// Get the Content-Range header value for a single range response.
    pub fn content_range_header(&self, content_length: u64) -> Result<String, RangeError> {
        if self.ranges.len() != 1 {
            return Err(RangeError::InvalidRange);
        }
        let (start, end) = self.ranges[0].resolve(content_length)?;
        Ok(format!("bytes {start}-{end}/{content_length}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_range() {
        let rs = RangeSet::parse("bytes=0-499").unwrap();
        assert_eq!(rs.ranges.len(), 1);
        let (start, end) = rs.ranges[0].resolve(1000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 499);
    }

    #[test]
    fn test_parse_open_end() {
        let rs = RangeSet::parse("bytes=500-").unwrap();
        let (start, end) = rs.ranges[0].resolve(1000).unwrap();
        assert_eq!(start, 500);
        assert_eq!(end, 999);
    }

    #[test]
    fn test_parse_suffix() {
        let rs = RangeSet::parse("bytes=-500").unwrap();
        let (start, end) = rs.ranges[0].resolve(1000).unwrap();
        assert_eq!(start, 500);
        assert_eq!(end, 999);
    }

    #[test]
    fn test_parse_multiple_ranges() {
        let rs = RangeSet::parse("bytes=0-99, 200-299").unwrap();
        assert_eq!(rs.ranges.len(), 2);
    }

    #[test]
    fn test_not_satisfiable() {
        let rs = RangeSet::parse("bytes=1500-2000").unwrap();
        assert!(rs.ranges[0].resolve(1000).is_err());
    }

    #[test]
    fn test_content_range_header() {
        let rs = RangeSet::parse("bytes=0-499").unwrap();
        let header = rs.content_range_header(1000).unwrap();
        assert_eq!(header, "bytes 0-499/1000");
    }

    #[test]
    fn test_clamp_end() {
        let rs = RangeSet::parse("bytes=0-5000").unwrap();
        let (start, end) = rs.ranges[0].resolve(1000).unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 999);
    }
}
