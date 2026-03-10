use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use dashmap::DashMap;
use parking_lot::Mutex;
use rv_config::CacheConfig;
use rv_hash::HashSlinger;
use rv_hash::objhead::ObjHead;
use rv_http::vary::VaryMatcher;
use rv_log::LogWriter;
use rv_storage::{ObjCore, Stevedore};
use rv_types::vsl::Vxid;
use rv_types::{Digest, LogTag, ObjAttr, VtimDur, VtimReal};

use crate::ban::BanList;
use crate::error::CacheError;
use crate::expire::ExpiryManager;
use crate::lookup::{CacheLookupResult, evaluate_object};
use crate::stats::CacheStats;

/// Default maximum number of cached objects before LRU eviction begins.
const DEFAULT_MAX_OBJECTS: usize = 100_000;

/// TTL parameters for object insertion.
pub struct TtlInfo {
    pub ttl: VtimDur,
    pub grace: VtimDur,
    pub keep: VtimDur,
}

impl Default for TtlInfo {
    fn default() -> Self {
        Self {
            ttl: VtimDur::from_secs(120.0),
            grace: VtimDur::from_secs(10.0),
            keep: VtimDur::ZERO,
        }
    }
}

/// Simple LRU eviction tracker using a VecDeque.
///
/// Digests are ordered from oldest (front) to newest (back).
/// When the cache exceeds `capacity`, `evict_oldest()` returns the
/// least-recently-used digest so the caller can remove it from storage.
///
/// This uses O(n) scans for `touch()` and `remove()` which is acceptable
/// for moderate cache sizes (up to hundreds of thousands of objects).
/// For significantly larger caches a doubly-linked-list with a HashMap
/// index would be more appropriate.
pub struct LruTracker {
    order: Mutex<VecDeque<Digest>>,
    capacity: usize,
}

impl LruTracker {
    /// Create a new LRU tracker with the given maximum capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            order: Mutex::new(VecDeque::with_capacity(capacity.min(8192))),
            capacity,
        }
    }

    /// Record a new digest insertion.  The digest is placed at the back
    /// of the queue (most-recently-used position).
    pub fn insert(&self, digest: Digest) {
        let mut order = self.order.lock();
        // Avoid duplicates -- if the digest already exists, move it to the back.
        if let Some(pos) = order.iter().position(|d| *d == digest) {
            order.remove(pos);
        }
        order.push_back(digest);
    }

    /// Touch a digest on access (cache hit), moving it to the
    /// most-recently-used position.
    pub fn touch(&self, digest: &Digest) {
        let mut order = self.order.lock();
        if let Some(pos) = order.iter().position(|d| d == digest) {
            order.remove(pos);
            order.push_back(*digest);
        }
    }

    /// Remove a specific digest (e.g. on purge or explicit expiry).
    pub fn remove(&self, digest: &Digest) {
        let mut order = self.order.lock();
        if let Some(pos) = order.iter().position(|d| d == digest) {
            order.remove(pos);
        }
    }

    /// Evict the least-recently-used digest (front of the queue).
    /// Returns `None` if the tracker is empty.
    pub fn evict_oldest(&self) -> Option<Digest> {
        let mut order = self.order.lock();
        order.pop_front()
    }

    /// The maximum number of objects this tracker allows before
    /// eviction should be triggered.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current number of tracked digests.
    pub fn len(&self) -> usize {
        self.order.lock().len()
    }

    /// Returns true if no digests are tracked.
    pub fn is_empty(&self) -> bool {
        self.order.lock().is_empty()
    }
}

/// The central cache engine that coordinates hash lookup, storage,
/// TTL/expiry management, ban processing, and LRU eviction.
pub struct CacheEngine {
    hash: Arc<dyn HashSlinger>,
    storage: Arc<dyn Stevedore>,
    ban_list: Arc<BanList>,
    expiry: Arc<ExpiryManager>,
    stats: Arc<CacheStats>,
    log: Arc<LogWriter>,
    config: CacheConfig,
    /// Direct digest -> variant list mapping for cache hits.
    /// Each digest can map to multiple variants (e.g. different Accept-Encoding).
    objects: DashMap<Digest, Vec<Arc<ObjCore>>>,
    /// LRU eviction tracker.
    lru: LruTracker,
    /// Maximum number of objects before LRU eviction kicks in.
    max_objects: usize,
}

impl CacheEngine {
    pub fn new(
        config: CacheConfig,
        hash: Arc<dyn HashSlinger>,
        storage: Arc<dyn Stevedore>,
        log: Arc<LogWriter>,
    ) -> Self {
        hash.start();

        Self {
            hash,
            storage,
            ban_list: Arc::new(BanList::new()),
            expiry: Arc::new(ExpiryManager::new()),
            stats: Arc::new(CacheStats::new()),
            log,
            config,
            objects: DashMap::new(),
            lru: LruTracker::new(DEFAULT_MAX_OBJECTS),
            max_objects: DEFAULT_MAX_OBJECTS,
        }
    }

    /// Create a cache engine with a custom maximum object count for LRU
    /// eviction.
    pub fn with_max_objects(
        config: CacheConfig,
        hash: Arc<dyn HashSlinger>,
        storage: Arc<dyn Stevedore>,
        log: Arc<LogWriter>,
        max_objects: usize,
    ) -> Self {
        hash.start();

        Self {
            hash,
            storage,
            ban_list: Arc::new(BanList::new()),
            expiry: Arc::new(ExpiryManager::new()),
            stats: Arc::new(CacheStats::new()),
            log,
            config,
            objects: DashMap::new(),
            lru: LruTracker::new(max_objects),
            max_objects,
        }
    }

    /// Look up an object by its digest.
    ///
    /// When `request_headers` is provided, the lookup performs Vary matching
    /// against each stored variant.  When it is `None`, the first non-expired
    /// variant is returned (backward-compatible behaviour).
    pub fn lookup(
        &self,
        digest: &Digest,
        request_headers: Option<&rv_http::header::HeaderMap>,
    ) -> CacheLookupResult {
        // Check the object map for cached variants
        if let Some(entry) = self.objects.get(digest) {
            let variants = entry.value();
            let now = VtimReal::now();

            // Track the best grace candidate while scanning
            let mut grace_candidate: Option<Arc<ObjCore>> = None;

            for oc in variants {
                // If request headers were supplied and this variant carries
                // Vary data, verify the request matches before considering it.
                if let Some(req_hdrs) = request_headers {
                    if let Some(vary_data) = oc.get_attr(ObjAttr::Vary) {
                        if !VaryMatcher::matches(&vary_data, req_hdrs) {
                            continue;
                        }
                    }
                }

                let result = evaluate_object(oc, now);
                match &result {
                    CacheLookupResult::Hit(_) => {
                        self.stats.cache_hit.fetch_add(1, Ordering::Relaxed);
                        // Touch the digest in the LRU tracker on cache hit
                        self.lru.touch(digest);
                        return result;
                    }
                    CacheLookupResult::Grace(_) => {
                        if grace_candidate.is_none() {
                            grace_candidate = Some(Arc::clone(oc));
                        }
                    }
                    _ => {}
                }
            }

            // Return the best grace hit if we found one
            if let Some(oc) = grace_candidate {
                self.stats.cache_hit_grace.fetch_add(1, Ordering::Relaxed);
                // Touch the digest in the LRU tracker on grace hit
                self.lru.touch(digest);
                return CacheLookupResult::Grace(oc);
            }

            // All variants expired or none matched -- clean up empty lists
            drop(entry);
            self.objects.remove(digest);
            self.lru.remove(digest);
        }

        self.stats.cache_miss.fetch_add(1, Ordering::Relaxed);
        CacheLookupResult::Miss
    }

    /// Look up using a pre-built ObjCore (for direct object evaluation).
    pub fn evaluate(&self, oc: &Arc<ObjCore>) -> CacheLookupResult {
        let now = VtimReal::now();
        let result = evaluate_object(oc, now);

        match &result {
            CacheLookupResult::Hit(_) => {
                self.stats.cache_hit.fetch_add(1, Ordering::Relaxed);
            }
            CacheLookupResult::Grace(_) => {
                self.stats.cache_hit_grace.fetch_add(1, Ordering::Relaxed);
            }
            CacheLookupResult::HitForPass => {
                self.stats
                    .cache_hit_for_pass
                    .fetch_add(1, Ordering::Relaxed);
            }
            CacheLookupResult::Miss => {
                self.stats.cache_miss.fetch_add(1, Ordering::Relaxed);
            }
            CacheLookupResult::Busy => {}
        }

        result
    }

    /// Insert an object into the cache.
    ///
    /// When `vary_data` is `Some`, the data is stored as an `ObjAttr::Vary`
    /// attribute on the object before it is added to the variant list.
    /// Multiple objects with different vary data can coexist under the same
    /// digest, enabling content negotiation (e.g. different Accept-Encoding
    /// representations).
    ///
    /// If the total object count exceeds `max_objects`, LRU eviction is
    /// performed to make room.
    pub fn insert(
        &self,
        digest: Digest,
        body: &[u8],
        ttl_info: TtlInfo,
        vary_data: Option<Vec<u8>>,
    ) -> Result<Arc<ObjCore>, CacheError> {
        // Evict oldest objects if we are at or above capacity
        self.enforce_capacity();

        let mut oc = ObjCore::new(digest);

        // Allocate storage
        self.storage
            .alloc_obj(&mut oc, body.len())
            .map_err(CacheError::Storage)?;

        // Set TTL parameters
        let now = VtimReal::now();
        oc.t_origin = now;
        oc.ttl = ttl_info.ttl;
        oc.grace = ttl_info.grace;
        oc.keep = ttl_info.keep;

        // Store the body in the stevedore and in the ObjCore itself so that
        // callers holding an Arc<ObjCore> can read it without going through the
        // storage backend.
        self.storage
            .extend(&oc, body)
            .map_err(CacheError::Storage)?;
        oc.store_body(body);

        // Store vary data as an attribute if provided
        if let Some(data) = &vary_data {
            oc.set_attr(ObjAttr::Vary, data);
        }

        let oc = Arc::new(oc);

        // Push to the variant list (do not replace the whole entry)
        self.objects
            .entry(digest)
            .or_default()
            .push(Arc::clone(&oc));

        // Track in LRU (insert moves to most-recently-used)
        self.lru.insert(digest);

        // Add to expiry queue
        self.expiry.insert(Arc::clone(&oc));

        // Update stats
        self.stats.n_objects.fetch_add(1, Ordering::Relaxed);
        self.stats
            .bytes_stored
            .fetch_add(body.len() as u64, Ordering::Relaxed);

        self.log.log(
            LogTag::ObjHeader,
            Vxid(0),
            format!("inserted object, ttl={}", ttl_info.ttl),
        );

        Ok(oc)
    }

    /// Remove an object (all variants) from the cache by its digest.
    ///
    /// This removes the object from the objects map, the LRU tracker,
    /// and frees the underlying storage.  Returns the number of
    /// variants that were removed.
    pub fn remove(&self, digest: &Digest) -> usize {
        let removed = self.objects.remove(digest);
        self.lru.remove(digest);

        let count = match &removed {
            Some((_, variants)) => variants.len(),
            None => 0,
        };

        if count > 0 {
            // Free storage for each variant
            if let Some((_, variants)) = removed {
                for _oc in &variants {
                    self.storage.free_obj(&mut ObjCore::new(*digest));
                }
            }

            self.stats
                .evictions
                .fetch_add(count as u64, Ordering::Relaxed);
        }

        count
    }

    /// Purge an object from the cache by digest.
    pub fn purge(&self, digest: &Digest) {
        let new_oh = Arc::new(ObjHead::new(*digest));
        let (oh, _) = self.hash.lookup(digest, new_oh);
        self.hash.deref(&oh);
        // Remove from the objects map and LRU tracker
        self.objects.remove(digest);
        self.lru.remove(digest);
        self.stats.n_purged.fetch_add(1, Ordering::Relaxed);
    }

    /// Add a ban expression.
    pub fn ban(&self, expression: &str) -> Result<(), CacheError> {
        self.ban_list.add_ban(expression)?;
        self.stats.bans_added.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Run one expiry cycle, evicting expired objects.
    pub fn expire_objects(&self) -> usize {
        let now = VtimReal::now();
        let expired = self.expiry.expire(now);
        let count = expired.len();

        for oc in &expired {
            // Remove from the LRU tracker
            self.lru.remove(&oc.digest);
            // Remove from the objects map
            self.objects.remove(&oc.digest);
            self.storage.free_obj(&mut ObjCore::new(oc.digest));
        }

        if count > 0 {
            self.stats
                .n_expired
                .fetch_add(count as u64, Ordering::Relaxed);
            self.stats
                .evictions
                .fetch_add(count as u64, Ordering::Relaxed);
        }

        count
    }

    /// Get a snapshot of cache statistics.
    pub fn stats(&self) -> crate::stats::CacheStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get a reference to the stats for direct atomic access.
    pub fn stats_ref(&self) -> &Arc<CacheStats> {
        &self.stats
    }

    /// Get a reference to the ban list.
    pub fn ban_list(&self) -> &Arc<BanList> {
        &self.ban_list
    }

    /// Get a reference to the expiry manager.
    pub fn expiry_manager(&self) -> &Arc<ExpiryManager> {
        &self.expiry
    }

    /// Get a reference to the storage backend.
    pub fn storage(&self) -> &Arc<dyn Stevedore> {
        &self.storage
    }

    /// Get a reference to the config.
    pub fn config(&self) -> &CacheConfig {
        &self.config
    }

    /// Get a reference to the LRU tracker.
    pub fn lru(&self) -> &LruTracker {
        &self.lru
    }

    /// The configured maximum object count.
    pub fn max_objects(&self) -> usize {
        self.max_objects
    }

    /// Enforce the maximum object capacity by evicting the
    /// least-recently-used objects until we are below the limit.
    fn enforce_capacity(&self) {
        while self.lru.len() >= self.max_objects {
            if let Some(victim) = self.lru.evict_oldest() {
                if let Some((_, variants)) = self.objects.remove(&victim) {
                    for _oc in &variants {
                        self.storage.free_obj(&mut ObjCore::new(victim));
                    }
                    self.stats
                        .evictions
                        .fetch_add(variants.len() as u64, Ordering::Relaxed);
                }
            } else {
                // No more entries to evict
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rv_hash::simple::SimpleListHash;
    use rv_http::header::HeaderMap;
    use rv_http::vary::VaryMatcher;
    use rv_log::RingBuffer;
    use rv_storage::malloc::MallocStevedore;

    fn make_engine() -> CacheEngine {
        let config = CacheConfig::default();
        let hash: Arc<dyn HashSlinger> = Arc::new(SimpleListHash::new());
        let storage: Arc<dyn Stevedore> = Arc::new(MallocStevedore::new("test", 256 * 1024 * 1024));
        let ringbuf = Arc::new(RingBuffer::new(1024));
        let log = Arc::new(LogWriter::new(ringbuf));
        CacheEngine::new(config, hash, storage, log)
    }

    fn make_engine_with_max(max_objects: usize) -> CacheEngine {
        let config = CacheConfig::default();
        let hash: Arc<dyn HashSlinger> = Arc::new(SimpleListHash::new());
        let storage: Arc<dyn Stevedore> = Arc::new(MallocStevedore::new("test", 256 * 1024 * 1024));
        let ringbuf = Arc::new(RingBuffer::new(1024));
        let log = Arc::new(LogWriter::new(ringbuf));
        CacheEngine::with_max_objects(config, hash, storage, log, max_objects)
    }

    fn test_digest(val: u8) -> Digest {
        let mut bytes = [0u8; 32];
        bytes[0] = val;
        Digest::new(bytes)
    }

    #[test]
    fn test_insert_and_stats() {
        let engine = make_engine();

        let result = engine.insert(test_digest(1), b"hello world", TtlInfo::default(), None);
        assert!(result.is_ok());

        let stats = engine.stats();
        assert_eq!(stats.n_objects, 1);
        assert_eq!(stats.bytes_stored, 11);
    }

    #[test]
    fn test_lookup_miss() {
        let engine = make_engine();
        match engine.lookup(&test_digest(99), None) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss, got {:?}", other),
        }
        assert_eq!(engine.stats().cache_miss, 1);
    }

    #[test]
    fn test_ban() {
        let engine = make_engine();
        assert!(engine.ban("req.url ~ ^/images/").is_ok());
        assert_eq!(engine.stats().bans_added, 1);
        assert_eq!(engine.ban_list().len(), 1);
    }

    #[test]
    fn test_purge() {
        let engine = make_engine();
        engine.purge(&test_digest(1));
        assert_eq!(engine.stats().n_purged, 1);
    }

    #[test]
    fn test_evaluate_object() {
        let engine = make_engine();

        let mut oc = ObjCore::new(test_digest(1));
        oc.t_origin = VtimReal::now();
        oc.ttl = VtimDur::from_secs(3600.0);
        let oc = Arc::new(oc);

        match engine.evaluate(&oc) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit, got {:?}", other),
        }
        assert_eq!(engine.stats().cache_hit, 1);
    }

    // -- Vary handling tests ------------------------------------------------

    #[test]
    fn test_insert_and_lookup_without_vary() {
        // When no vary data is stored, any lookup (with or without request
        // headers) should return the cached object.
        let engine = make_engine();
        let digest = test_digest(10);

        engine
            .insert(digest, b"plain body", TtlInfo::default(), None)
            .unwrap();

        // Lookup without request headers
        match engine.lookup(&digest, None) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit, got {:?}", other),
        }

        // Lookup with request headers -- no Vary data on the object, so it
        // matches regardless of what the request carries.
        let mut req = HeaderMap::new();
        req.set("Accept-Encoding", "br");
        match engine.lookup(&digest, Some(&req)) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit, got {:?}", other),
        }
    }

    #[test]
    fn test_vary_different_accept_encoding_cached_separately() {
        let engine = make_engine();
        let digest = test_digest(20);

        // Build vary data for a gzip variant
        let mut gzip_req = HeaderMap::new();
        gzip_req.set("Accept-Encoding", "gzip");
        let gzip_vary = VaryMatcher::build_vary_data(Some("Accept-Encoding"), &gzip_req).unwrap();

        // Build vary data for a brotli variant
        let mut br_req = HeaderMap::new();
        br_req.set("Accept-Encoding", "br");
        let br_vary = VaryMatcher::build_vary_data(Some("Accept-Encoding"), &br_req).unwrap();

        // Insert both variants under the same digest
        engine
            .insert(
                digest,
                b"gzip-compressed body",
                TtlInfo::default(),
                Some(gzip_vary),
            )
            .unwrap();
        engine
            .insert(
                digest,
                b"brotli-compressed body",
                TtlInfo::default(),
                Some(br_vary),
            )
            .unwrap();

        assert_eq!(engine.stats().n_objects, 2);

        // Lookup with gzip request -- must return the gzip variant
        match engine.lookup(&digest, Some(&gzip_req)) {
            CacheLookupResult::Hit(oc) => {
                let body = oc.get_body().expect("body should exist");
                assert_eq!(&body[..], b"gzip-compressed body");
            }
            other => panic!("expected Hit for gzip, got {:?}", other),
        }

        // Lookup with br request -- must return the brotli variant
        match engine.lookup(&digest, Some(&br_req)) {
            CacheLookupResult::Hit(oc) => {
                let body = oc.get_body().expect("body should exist");
                assert_eq!(&body[..], b"brotli-compressed body");
            }
            other => panic!("expected Hit for br, got {:?}", other),
        }

        // Lookup with a completely different Accept-Encoding -- miss
        let mut deflate_req = HeaderMap::new();
        deflate_req.set("Accept-Encoding", "deflate");
        match engine.lookup(&digest, Some(&deflate_req)) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss for deflate, got {:?}", other),
        }
    }

    #[test]
    fn test_vary_star_never_matches() {
        let engine = make_engine();
        let digest = test_digest(30);

        // Vary: * produces a special marker that never matches
        let req = HeaderMap::new();
        let vary_star = VaryMatcher::build_vary_data(Some("*"), &req).unwrap();

        engine
            .insert(
                digest,
                b"uncacheable body",
                TtlInfo::default(),
                Some(vary_star),
            )
            .unwrap();

        // Lookup with any request headers should miss (Vary: * never matches)
        let mut some_req = HeaderMap::new();
        some_req.set("Accept-Encoding", "gzip");
        match engine.lookup(&digest, Some(&some_req)) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss for Vary:*, got {:?}", other),
        }

        // Even without headers it is a miss when Vary: * is present, because
        // None means "skip vary checking" -- the object is still returned.
        // But with an empty HeaderMap the Vary:* marker rejects the match.
        match engine.lookup(&digest, Some(&HeaderMap::new())) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss for Vary:* (empty headers), got {:?}", other),
        }
    }

    #[test]
    fn test_lookup_without_request_headers_returns_first_variant() {
        // Backward compatibility: when no request headers are supplied,
        // the lookup should return the first non-expired variant.
        let engine = make_engine();
        let digest = test_digest(40);

        let mut gzip_req = HeaderMap::new();
        gzip_req.set("Accept-Encoding", "gzip");
        let gzip_vary = VaryMatcher::build_vary_data(Some("Accept-Encoding"), &gzip_req).unwrap();

        engine
            .insert(
                digest,
                b"first variant",
                TtlInfo::default(),
                Some(gzip_vary),
            )
            .unwrap();

        // No request headers -- should still return a hit (first variant)
        match engine.lookup(&digest, None) {
            CacheLookupResult::Hit(oc) => {
                let body = oc.get_body().expect("body should exist");
                assert_eq!(&body[..], b"first variant");
            }
            other => panic!("expected Hit (backward compat), got {:?}", other),
        }
    }

    #[test]
    fn test_multiple_variants_stats() {
        let engine = make_engine();
        let digest = test_digest(50);

        let mut gzip_req = HeaderMap::new();
        gzip_req.set("Accept-Encoding", "gzip");
        let gzip_vary = VaryMatcher::build_vary_data(Some("Accept-Encoding"), &gzip_req).unwrap();

        let mut br_req = HeaderMap::new();
        br_req.set("Accept-Encoding", "br");
        let br_vary = VaryMatcher::build_vary_data(Some("Accept-Encoding"), &br_req).unwrap();

        engine
            .insert(digest, b"gzip", TtlInfo::default(), Some(gzip_vary))
            .unwrap();
        engine
            .insert(digest, b"br", TtlInfo::default(), Some(br_vary))
            .unwrap();

        // Both inserts counted
        assert_eq!(engine.stats().n_objects, 2);
        assert_eq!(engine.stats().bytes_stored, 6); // "gzip" + "br"

        // Hit for gzip
        engine.lookup(&digest, Some(&gzip_req));
        assert_eq!(engine.stats().cache_hit, 1);

        // Hit for br
        engine.lookup(&digest, Some(&br_req));
        assert_eq!(engine.stats().cache_hit, 2);
    }

    // -- LRU eviction tests -------------------------------------------------

    #[test]
    fn test_lru_tracker_basic() {
        let lru = LruTracker::new(10);
        assert!(lru.is_empty());
        assert_eq!(lru.len(), 0);

        let d1 = test_digest(1);
        let d2 = test_digest(2);

        lru.insert(d1);
        lru.insert(d2);
        assert_eq!(lru.len(), 2);

        // Evict oldest returns d1 (inserted first)
        assert_eq!(lru.evict_oldest(), Some(d1));
        assert_eq!(lru.len(), 1);

        assert_eq!(lru.evict_oldest(), Some(d2));
        assert!(lru.is_empty());

        assert_eq!(lru.evict_oldest(), None);
    }

    #[test]
    fn test_lru_tracker_touch_moves_to_back() {
        let lru = LruTracker::new(10);
        let d1 = test_digest(1);
        let d2 = test_digest(2);
        let d3 = test_digest(3);

        lru.insert(d1);
        lru.insert(d2);
        lru.insert(d3);

        // Touch d1 -- it should move to the back
        lru.touch(&d1);

        // Evict order should now be d2, d3, d1
        assert_eq!(lru.evict_oldest(), Some(d2));
        assert_eq!(lru.evict_oldest(), Some(d3));
        assert_eq!(lru.evict_oldest(), Some(d1));
    }

    #[test]
    fn test_lru_tracker_remove() {
        let lru = LruTracker::new(10);
        let d1 = test_digest(1);
        let d2 = test_digest(2);

        lru.insert(d1);
        lru.insert(d2);

        lru.remove(&d1);
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.evict_oldest(), Some(d2));
    }

    #[test]
    fn test_lru_tracker_insert_deduplicates() {
        let lru = LruTracker::new(10);
        let d1 = test_digest(1);

        lru.insert(d1);
        lru.insert(d1); // duplicate
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn test_lru_eviction_on_insert() {
        // Create an engine with max 3 objects
        let engine = make_engine_with_max(3);

        engine
            .insert(test_digest(1), b"one", TtlInfo::default(), None)
            .unwrap();
        engine
            .insert(test_digest(2), b"two", TtlInfo::default(), None)
            .unwrap();
        engine
            .insert(test_digest(3), b"three", TtlInfo::default(), None)
            .unwrap();

        assert_eq!(engine.lru().len(), 3);

        // Inserting a 4th object should evict the oldest (digest 1)
        engine
            .insert(test_digest(4), b"four", TtlInfo::default(), None)
            .unwrap();

        assert_eq!(engine.lru().len(), 3);

        // digest 1 should be gone
        match engine.lookup(&test_digest(1), None) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss after LRU eviction, got {:?}", other),
        }

        // digests 2, 3, 4 should still be present
        match engine.lookup(&test_digest(2), None) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit for digest 2, got {:?}", other),
        }
        match engine.lookup(&test_digest(3), None) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit for digest 3, got {:?}", other),
        }
        match engine.lookup(&test_digest(4), None) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit for digest 4, got {:?}", other),
        }
    }

    #[test]
    fn test_lru_touch_on_hit_prevents_eviction() {
        // Create an engine with max 3 objects
        let engine = make_engine_with_max(3);

        engine
            .insert(test_digest(1), b"one", TtlInfo::default(), None)
            .unwrap();
        engine
            .insert(test_digest(2), b"two", TtlInfo::default(), None)
            .unwrap();
        engine
            .insert(test_digest(3), b"three", TtlInfo::default(), None)
            .unwrap();

        // Touch digest 1 by looking it up (moves to back of LRU)
        match engine.lookup(&test_digest(1), None) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit, got {:?}", other),
        }

        // Insert a 4th object -- should evict digest 2 (now the oldest)
        engine
            .insert(test_digest(4), b"four", TtlInfo::default(), None)
            .unwrap();

        // digest 1 should still be present (it was touched)
        match engine.lookup(&test_digest(1), None) {
            CacheLookupResult::Hit(_) => {}
            other => panic!("expected Hit for touched digest 1, got {:?}", other),
        }

        // digest 2 should be evicted
        match engine.lookup(&test_digest(2), None) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss for evicted digest 2, got {:?}", other),
        }
    }

    #[test]
    fn test_remove_method() {
        let engine = make_engine();
        let digest = test_digest(1);

        engine
            .insert(digest, b"data", TtlInfo::default(), None)
            .unwrap();

        assert_eq!(engine.lru().len(), 1);

        let removed = engine.remove(&digest);
        assert_eq!(removed, 1);
        assert_eq!(engine.lru().len(), 0);

        match engine.lookup(&digest, None) {
            CacheLookupResult::Miss => {}
            other => panic!("expected Miss after remove, got {:?}", other),
        }
    }

    #[test]
    fn test_remove_nonexistent_returns_zero() {
        let engine = make_engine();
        assert_eq!(engine.remove(&test_digest(99)), 0);
    }

    #[test]
    fn test_purge_removes_from_lru() {
        let engine = make_engine();
        let digest = test_digest(1);

        engine
            .insert(digest, b"data", TtlInfo::default(), None)
            .unwrap();
        assert_eq!(engine.lru().len(), 1);

        engine.purge(&digest);
        assert_eq!(engine.lru().len(), 0);
    }
}
