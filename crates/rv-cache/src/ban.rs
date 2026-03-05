use parking_lot::RwLock;
use regex::Regex;
use rv_types::VtimReal;

use rv_storage::ObjCore;

use crate::error::CacheError;

/// A ban test operation.
#[derive(Debug, Clone)]
pub enum BanTest {
    /// Match URL against a regex pattern.
    UrlMatch(Regex),
    /// Match a specific response header against a regex pattern.
    ObjHeaderMatch { header: String, pattern: Regex },
    /// Match a specific response header for equality.
    ObjHeaderEq { header: String, value: String },
}

/// A single ban entry.
#[derive(Debug, Clone)]
pub struct Ban {
    /// When the ban was created.
    pub timestamp: VtimReal,
    /// Tests that must all match for the ban to apply.
    pub tests: Vec<BanTest>,
    /// Whether this ban has been fully checked by the lurker.
    pub completed: bool,
}

impl Ban {
    /// Check if an object matches this ban.
    pub fn matches(&self, url: &str, get_header: &dyn Fn(&str) -> Option<String>) -> bool {
        self.tests.iter().all(|test| match test {
            BanTest::UrlMatch(re) => re.is_match(url),
            BanTest::ObjHeaderMatch { header, pattern } => {
                get_header(header).is_some_and(|v| pattern.is_match(&v))
            }
            BanTest::ObjHeaderEq { header, value } => {
                get_header(header).is_some_and(|v| v == *value)
            }
        })
    }
}

/// Thread-safe ban list manager.
pub struct BanList {
    bans: RwLock<Vec<Ban>>,
}

impl BanList {
    pub fn new() -> Self {
        Self {
            bans: RwLock::new(Vec::new()),
        }
    }

    /// Add a ban from a ban expression string.
    /// Format: `req.url ~ /pattern/` or `obj.http.X-Header == value`
    pub fn add_ban(&self, expression: &str) -> Result<(), CacheError> {
        let ban = parse_ban_expression(expression)?;
        let mut bans = self.bans.write();
        bans.push(ban);
        Ok(())
    }

    /// Add a pre-built ban.
    pub fn add(&self, ban: Ban) {
        let mut bans = self.bans.write();
        bans.push(ban);
    }

    /// Check if an object is banned.
    /// Returns true if the object matches any active ban created after its origin time.
    pub fn is_banned(
        &self,
        oc: &ObjCore,
        url: &str,
        get_header: &dyn Fn(&str) -> Option<String>,
    ) -> bool {
        let bans = self.bans.read();
        bans.iter().any(|ban| {
            // Only check bans created after the object was stored
            ban.timestamp.0 > oc.t_origin.0 && ban.matches(url, get_header)
        })
    }

    /// Remove bans that are older than the oldest object in cache.
    /// Called by the ban lurker background task.
    pub fn lurker_work(&self, oldest_obj_time: VtimReal) {
        let mut bans = self.bans.write();
        bans.retain(|ban| ban.timestamp.0 >= oldest_obj_time.0 || !ban.completed);
    }

    /// Mark all bans as completed (lurker has checked all objects against them).
    pub fn mark_completed(&self) {
        let mut bans = self.bans.write();
        for ban in bans.iter_mut() {
            ban.completed = true;
        }
    }

    /// Get the number of active bans.
    pub fn len(&self) -> usize {
        self.bans.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.bans.read().is_empty()
    }

    /// List all active bans (for admin CLI).
    pub fn list(&self) -> Vec<Ban> {
        self.bans.read().clone()
    }
}

impl Default for BanList {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a ban expression string into a Ban.
///
/// Supported formats:
/// - `req.url ~ /pattern/` - URL regex match
/// - `obj.http.Header ~ /pattern/` - response header regex match
/// - `obj.http.Header == value` - response header equality
fn parse_ban_expression(expr: &str) -> Result<Ban, CacheError> {
    let mut tests = Vec::new();
    // Split by " && " for multiple conditions
    let parts: Vec<&str> = expr.split(" && ").collect();

    for part in parts {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("req.url ~ ") {
            let pattern = rest.trim();
            let re = Regex::new(pattern)
                .map_err(|e| CacheError::BanError(format!("invalid regex: {e}")))?;
            tests.push(BanTest::UrlMatch(re));
        } else if let Some(rest) = part.strip_prefix("obj.http.") {
            if let Some((header, pattern)) = rest.split_once(" ~ ") {
                let re = Regex::new(pattern.trim())
                    .map_err(|e| CacheError::BanError(format!("invalid regex: {e}")))?;
                tests.push(BanTest::ObjHeaderMatch {
                    header: header.trim().to_string(),
                    pattern: re,
                });
            } else if let Some((header, value)) = rest.split_once(" == ") {
                tests.push(BanTest::ObjHeaderEq {
                    header: header.trim().to_string(),
                    value: value.trim().to_string(),
                });
            } else {
                return Err(CacheError::BanError(format!("invalid ban test: {part}")));
            }
        } else {
            return Err(CacheError::BanError(format!("unknown ban field: {part}")));
        }
    }

    if tests.is_empty() {
        return Err(CacheError::BanError("empty ban expression".to_string()));
    }

    Ok(Ban {
        timestamp: VtimReal::now(),
        tests,
        completed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_types::Digest;

    fn test_digest(val: u8) -> Digest {
        let mut bytes = [0u8; 32];
        bytes[0] = val;
        Digest::new(bytes)
    }

    #[test]
    fn test_url_ban() {
        let ban_list = BanList::new();
        ban_list.add_ban("req.url ~ ^/images/").unwrap();

        let mut oc = ObjCore::new(test_digest(1));
        oc.t_origin = VtimReal::from_secs(0.0); // Before ban was created

        let get_header = |_: &str| -> Option<String> { None };
        assert!(ban_list.is_banned(&oc, "/images/logo.png", &get_header));
        assert!(!ban_list.is_banned(&oc, "/api/data", &get_header));
    }

    #[test]
    fn test_header_ban() {
        let ban_list = BanList::new();
        ban_list
            .add_ban("obj.http.Content-Type ~ text/html")
            .unwrap();

        let mut oc = ObjCore::new(test_digest(2));
        oc.t_origin = VtimReal::from_secs(0.0);

        let get_header_html = |name: &str| -> Option<String> {
            if name == "Content-Type" {
                Some("text/html".to_string())
            } else {
                None
            }
        };
        assert!(ban_list.is_banned(&oc, "/page", &get_header_html));

        let get_header_json = |name: &str| -> Option<String> {
            if name == "Content-Type" {
                Some("application/json".to_string())
            } else {
                None
            }
        };
        assert!(!ban_list.is_banned(&oc, "/page", &get_header_json));
    }

    #[test]
    fn test_compound_ban() {
        let ban_list = BanList::new();
        ban_list
            .add_ban("req.url ~ ^/api/ && obj.http.X-Cache == old")
            .unwrap();

        let mut oc = ObjCore::new(test_digest(3));
        oc.t_origin = VtimReal::from_secs(0.0);

        let get_header = |name: &str| -> Option<String> {
            if name == "X-Cache" {
                Some("old".to_string())
            } else {
                None
            }
        };

        // Both conditions match
        assert!(ban_list.is_banned(&oc, "/api/users", &get_header));
        // URL doesn't match
        assert!(!ban_list.is_banned(&oc, "/web/page", &get_header));
    }

    #[test]
    fn test_ban_timestamp_ordering() {
        let ban_list = BanList::new();
        ban_list.add_ban("req.url ~ .*").unwrap();

        // Object created AFTER the ban
        let mut oc = ObjCore::new(test_digest(4));
        oc.t_origin = VtimReal::now();

        let get_header = |_: &str| -> Option<String> { None };
        // Should NOT be banned because object was created after the ban
        assert!(!ban_list.is_banned(&oc, "/anything", &get_header));
    }
}
