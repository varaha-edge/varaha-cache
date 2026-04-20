use std::sync::Arc;

use rv_types::{ObjCoreFlags, VtimReal};

use rv_storage::ObjCore;

/// Result of a cache lookup.
#[derive(Debug)]
pub enum CacheLookupResult {
    /// Fresh cache hit - object is valid and within TTL.
    Hit(Arc<ObjCore>),

    /// Cache miss - no matching object found.
    Miss,

    /// Grace hit - object is expired but within grace period.
    /// The caller should serve this stale object while fetching a fresh copy.
    Grace(Arc<ObjCore>),

    /// Hit-for-pass - a previous lookup determined this object is uncacheable.
    /// The request should be passed directly to the backend.
    HitForPass,

    /// The object is currently being fetched by another request.
    /// The caller should wait for the fetch to complete.
    Busy,
}

/// Evaluate an ObjCore to determine the cache lookup result.
pub fn evaluate_object(oc: &Arc<ObjCore>, now: VtimReal) -> CacheLookupResult {
    let flags = oc.oc_flags();

    // Check if the object is marked as busy (being fetched)
    if flags.contains(ObjCoreFlags::BUSY) {
        return CacheLookupResult::Busy;
    }

    // Check if the object is marked hit-for-miss/hit-for-pass
    if flags.contains(ObjCoreFlags::HFP) {
        return CacheLookupResult::HitForPass;
    }

    // Check if the object is dying/withdrawn
    if flags.contains(ObjCoreFlags::DYING) || flags.contains(ObjCoreFlags::WITHDRAWN) {
        return CacheLookupResult::Miss;
    }

    // Check TTL
    if !oc.is_expired(now) {
        return CacheLookupResult::Hit(Arc::clone(oc));
    }

    // Object is expired - check grace period
    let grace_remaining = oc.remaining_grace(now);
    if grace_remaining.is_positive() {
        return CacheLookupResult::Grace(Arc::clone(oc));
    }

    CacheLookupResult::Miss
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_types::{Digest, VtimDur};

    fn test_digest(val: u8) -> Digest {
        let mut bytes = [0u8; 32];
        bytes[0] = val;
        Digest::new(bytes)
    }

    #[test]
    fn test_evaluate_fresh_hit() {
        let mut oc = ObjCore::new(test_digest(1));
        oc.t_origin = VtimReal::from_secs(1000.0);
        oc.ttl = VtimDur::from_secs(60.0);
        oc.grace = VtimDur::from_secs(30.0);
        let oc = Arc::new(oc);

        let now = VtimReal::from_secs(1050.0); // Within TTL
        match evaluate_object(&oc, now) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit, got {other:?}"),
        }
    }

    #[test]
    fn test_evaluate_grace_hit() {
        let mut oc = ObjCore::new(test_digest(2));
        oc.t_origin = VtimReal::from_secs(1000.0);
        oc.ttl = VtimDur::from_secs(60.0);
        oc.grace = VtimDur::from_secs(30.0);
        let oc = Arc::new(oc);

        let now = VtimReal::from_secs(1070.0); // Past TTL, within grace
        match evaluate_object(&oc, now) {
            CacheLookupResult::Grace(_) => {}
            other => panic!("expected Grace, got {other:?}"),
        }
    }

    #[test]
    fn test_evaluate_miss() {
        let mut oc = ObjCore::new(test_digest(3));
        oc.t_origin = VtimReal::from_secs(1000.0);
        oc.ttl = VtimDur::from_secs(60.0);
        oc.grace = VtimDur::from_secs(30.0);
        let oc = Arc::new(oc);

        let now = VtimReal::from_secs(1200.0); // Past TTL + grace
        match evaluate_object(&oc, now) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss, got {other:?}"),
        }
    }

    #[test]
    fn test_evaluate_busy() {
        let oc = ObjCore::new(test_digest(4));
        oc.set_oc_flag(ObjCoreFlags::BUSY);
        let oc = Arc::new(oc);

        match evaluate_object(&oc, VtimReal::now()) {
            CacheLookupResult::Busy => {}
            other => panic!("expected Busy, got {other:?}"),
        }
    }

    #[test]
    fn test_evaluate_hfp() {
        let oc = ObjCore::new(test_digest(5));
        oc.set_oc_flag(ObjCoreFlags::HFP);
        let oc = Arc::new(oc);

        match evaluate_object(&oc, VtimReal::now()) {
            CacheLookupResult::HitForPass => {}
            other => panic!("expected HitForPass, got {other:?}"),
        }
    }
}
