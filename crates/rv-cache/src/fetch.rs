use std::sync::Arc;

use rv_types::{ObjCoreFlags, VtimDur, VtimReal};
use rv_types::vsl::Vxid;

use rv_http::message::HttpMessage;
use rv_http::rfc2616::TtlCalculation;
use rv_storage::ObjCore;

/// Context for a backend fetch operation.
pub struct FetchContext {
    /// Unique transaction ID for this fetch.
    pub vxid: Vxid,
    /// The backend request being sent.
    pub bereq: HttpMessage,
    /// The backend response received.
    pub beresp: Option<HttpMessage>,
    /// The ObjCore being populated.
    pub objcore: Option<Arc<ObjCore>>,
    /// Whether this fetch is for a pass (uncacheable).
    pub is_pass: bool,
    /// Whether the response body should be streamed to waiting clients.
    pub do_stream: bool,
    /// Timestamp when the fetch started.
    pub t_fetch: VtimReal,
}

impl FetchContext {
    pub fn new(bereq: HttpMessage, vxid: Vxid) -> Self {
        Self {
            vxid,
            bereq,
            beresp: None,
            objcore: None,
            is_pass: false,
            do_stream: false,
            t_fetch: VtimReal::now(),
        }
    }

    /// Apply TTL/grace/keep from the backend response to the ObjCore.
    pub fn apply_ttl(
        &self,
        oc: &mut ObjCore,
        beresp: &HttpMessage,
        default_ttl: VtimDur,
        default_grace: VtimDur,
        default_keep: VtimDur,
    ) {
        let now = VtimReal::now();

        // Calculate TTL from response headers per RFC 7234
        let ttl_calc = TtlCalculation::from_response(beresp, now, default_ttl, default_grace, default_keep);
        oc.t_origin = now;
        oc.ttl = ttl_calc.ttl;
        oc.grace = ttl_calc.grace;
        oc.keep = ttl_calc.keep;
    }

    /// Mark the ObjCore as a hit-for-pass object.
    pub fn mark_hit_for_pass(oc: &ObjCore) {
        oc.set_oc_flag(ObjCoreFlags::HFP);
    }

    /// Mark the fetch as complete (object is fully stored).
    pub fn finish_fetch(oc: &ObjCore) {
        oc.clear_oc_flag(ObjCoreFlags::BUSY);
    }

    /// Mark the fetch as failed.
    pub fn fail_fetch(oc: &ObjCore) {
        oc.set_oc_flag(ObjCoreFlags::FAILED);
        oc.clear_oc_flag(ObjCoreFlags::BUSY);
    }
}

/// Determine if a backend response is cacheable.
pub fn is_cacheable(beresp: &HttpMessage, is_pass: bool) -> bool {
    if is_pass {
        return false;
    }

    // Check status code - only cache certain status codes
    let code = beresp.status.code();
    if !matches!(
        code,
        200 | 203 | 204 | 300 | 301 | 302 | 304 | 307 | 308 | 404 | 410 | 414
    ) {
        return false;
    }

    // Check Cache-Control: no-store
    if let Some(cc) = beresp.headers.get("Cache-Control") {
        let cc_lower = cc.to_lowercase();
        if cc_lower.contains("no-store") || cc_lower.contains("private") {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_http::message::HttpVersion;
    use rv_types::{HttpMethod, HttpStatus};

    #[test]
    fn test_cacheable_200() {
        let resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        assert!(is_cacheable(&resp, false));
    }

    #[test]
    fn test_not_cacheable_pass() {
        let resp = HttpMessage::new_response(HttpStatus::OK, HttpVersion::Http11);
        assert!(!is_cacheable(&resp, true));
    }

    #[test]
    fn test_not_cacheable_500() {
        let resp = HttpMessage::new_response(
            HttpStatus::INTERNAL_SERVER_ERROR,
            HttpVersion::Http11,
        );
        assert!(!is_cacheable(&resp, false));
    }

    #[test]
    fn test_fetch_context_creation() {
        let req = HttpMessage::new_request(HttpMethod::Get, "/test", HttpVersion::Http11);
        let ctx = FetchContext::new(req, Vxid(42));
        assert_eq!(ctx.vxid, Vxid(42));
        assert!(!ctx.is_pass);
        assert!(ctx.beresp.is_none());
    }

    #[test]
    fn test_finish_fetch_clears_busy() {
        let oc = ObjCore::new(rv_types::Digest::new([0u8; 32]));
        oc.set_oc_flag(ObjCoreFlags::BUSY);
        assert!(oc.is_busy());

        FetchContext::finish_fetch(&oc);
        assert!(!oc.is_busy());
    }
}
