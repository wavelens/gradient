/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-memory tier for small NARs, ranked by hits per byte.
//!
//! Greedy-Dual-Size-Frequency: an entry's priority is a global age floor plus
//! its hits divided by its size, the lowest priority is evicted first, and the
//! floor rises to each victim's priority. A 1 MiB entry therefore needs a
//! hundred times the hits of a 10 KiB entry to hold its place and leaves first
//! when it does not get them, so one burst of big objects cannot flush the
//! small ones every builder asks for. Loads are single flight per hash.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use gradient_util::sync::Mutex;

pub(crate) const PRIORITY_SCALE: u64 = 1 << 32;

type LoadFuture = Shared<BoxFuture<'static, Result<Bytes, Arc<anyhow::Error>>>>;

struct Entry {
    bytes: Bytes,
    hits: u64,
    priority: u64,
    seq: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    order: BTreeMap<(u64, u64), String>,
    bytes: u64,
    floor: u64,
    seq: u64,
}

impl Inner {
    fn priority(&self, hits: u64, size: u64) -> u64 {
        let earned = (u128::from(hits) * u128::from(PRIORITY_SCALE)) / u128::from(size.max(1));

        self.floor
            .saturating_add(u64::try_from(earned).unwrap_or(u64::MAX))
    }

    fn remove(&mut self, hash: &str) -> Option<Entry> {
        let entry = self.entries.remove(hash)?;
        self.order.remove(&(entry.priority, entry.seq));
        self.bytes -= entry.bytes.len() as u64;

        Some(entry)
    }

    fn evict_one(&mut self) -> bool {
        let Some((&key, _)) = self.order.iter().next() else {
            return false;
        };

        let Some(hash) = self.order.remove(&key) else {
            return false;
        };

        if let Some(entry) = self.entries.remove(&hash) {
            self.bytes -= entry.bytes.len() as u64;
        }

        self.floor = self.floor.max(key.0);

        true
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HotNarStats {
    pub entries: u64,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

pub struct HotNarCache {
    inner: Option<Mutex<Inner>>,
    capacity: u64,
    small_nar_bytes: u64,
    loads: tokio::sync::Mutex<HashMap<String, LoadFuture>>,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

impl std::fmt::Debug for HotNarCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotNarCache")
            .field("capacity", &self.capacity)
            .field("small_nar_bytes", &self.small_nar_bytes)
            .finish_non_exhaustive()
    }
}

impl HotNarCache {
    pub fn new(capacity_bytes: u64, small_nar_bytes: u64) -> Self {
        Self {
            inner: (capacity_bytes > 0).then(|| Mutex::new(Inner::default())),
            capacity: capacity_bytes,
            small_nar_bytes,
            loads: tokio::sync::Mutex::new(HashMap::new()),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    pub fn disabled() -> Self {
        Self::new(0, 0)
    }

    pub fn admits(&self, size: u64) -> bool {
        self.inner.is_some() && size <= self.small_nar_bytes && size <= self.capacity
    }

    /// A lookup that re-ranks the entry but leaves the counters alone, for a
    /// re-probe behind an already-counted [`Self::get`]: one lookup is one hit or
    /// one miss however many times the code has to ask.
    fn peek(&self, hash: &str) -> Option<Bytes> {
        let inner = self.inner.as_ref()?;
        let mut inner = inner.lock();
        let entry = inner.entries.get(hash)?;

        let old = (entry.priority, entry.seq);
        let (hits, size) = (entry.hits + 1, entry.bytes.len() as u64);
        let priority = inner.priority(hits, size);
        inner.order.remove(&old);
        let entry = inner.entries.get_mut(hash).expect("looked up above");
        entry.hits = hits;
        entry.priority = priority;
        let (bytes, seq) = (entry.bytes.clone(), entry.seq);
        inner.order.insert((priority, seq), hash.to_owned());

        Some(bytes)
    }

    pub fn get(&self, hash: &str) -> Option<Bytes> {
        self.inner.as_ref()?;
        match self.peek(hash) {
            Some(bytes) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(bytes)
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Admit `bytes` under `hash`, evicting the lowest-ranked entries until it
    /// fits. A size the cache does not admit only drops the hash's old entry.
    pub fn insert(&self, hash: &str, bytes: Bytes) {
        let size = bytes.len() as u64;
        if !self.admits(size) {
            self.invalidate(hash);
            return;
        }

        let Some(inner) = self.inner.as_ref() else {
            return;
        };

        let mut inner = inner.lock();
        inner.remove(hash);
        while inner.bytes + size > self.capacity {
            if !inner.evict_one() {
                break;
            }

            self.evictions.fetch_add(1, Ordering::Relaxed);
        }

        inner.seq += 1;
        let (seq, priority) = (inner.seq, inner.priority(1, size));
        inner.order.insert((priority, seq), hash.to_owned());
        inner.entries.insert(
            hash.to_owned(),
            Entry {
                bytes,
                hits: 1,
                priority,
                seq,
            },
        );
        inner.bytes += size;
    }

    pub fn invalidate(&self, hash: &str) {
        if let Some(inner) = self.inner.as_ref() {
            inner.lock().remove(hash);
        }
    }

    /// A hit, or one run of `load` shared by every caller that misses on `hash`
    /// meanwhile; the result is admitted on success and never on failure.
    pub async fn get_or_load<F>(&self, hash: &str, load: F) -> anyhow::Result<Bytes>
    where
        F: Future<Output = anyhow::Result<Bytes>> + Send + 'static,
    {
        if let Some(bytes) = self.peek(hash) {
            return Ok(bytes);
        }

        let (shared, leader) = {
            let mut loads = self.loads.lock().await;
            match loads.get(hash) {
                Some(running) => (running.clone(), false),
                None => {
                    let shared = load.map(|r| r.map_err(Arc::new)).boxed().shared();
                    loads.insert(hash.to_owned(), shared.clone());
                    (shared, true)
                }
            }
        };

        let result = shared.await;
        if leader {
            self.loads.lock().await.remove(hash);
            if let Ok(bytes) = &result {
                self.insert(hash, bytes.clone());
            }
        }

        result.map_err(|e| anyhow::anyhow!("{e:#}"))
    }

    pub fn stats(&self) -> HotNarStats {
        let (entries, bytes) = self
            .inner
            .as_ref()
            .map(|inner| {
                let inner = inner.lock();
                (inner.entries.len() as u64, inner.bytes)
            })
            .unwrap_or_default();

        HotNarStats {
            entries,
            bytes,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HotNarCache;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;

    fn cache(capacity: u64, threshold: u64) -> HotNarCache {
        HotNarCache::new(capacity, threshold)
    }

    fn blob(size: u64, fill: u8) -> Bytes {
        Bytes::from(vec![fill; size as usize])
    }

    #[test]
    fn admission_is_bounded_by_the_threshold_and_the_capacity() {
        let c = cache(64 * KIB, MIB);
        assert!(c.admits(64 * KIB));
        assert!(!c.admits(64 * KIB + 1), "over capacity");
        let c = cache(MIB, 10 * KIB);
        assert!(!c.admits(10 * KIB + 1), "over the small-NAR threshold");
        assert!(!HotNarCache::disabled().admits(1));
    }

    #[test]
    fn the_byte_total_never_exceeds_the_capacity() {
        let c = cache(100 * KIB, MIB);
        for i in 0..20u8 {
            c.insert(&format!("h{i}"), blob(10 * KIB, i));
        }

        let stats = c.stats();
        assert!(stats.bytes <= 100 * KIB, "{stats:?}");
        assert_eq!(stats.entries, 10);
        assert_eq!(stats.evictions, 10);
    }

    /// The crowding rule: a fresh 1 MiB entry ranks below a hundred fresh
    /// 10 KiB entries, so it is the victim when the next small one arrives.
    #[test]
    fn a_big_insert_is_the_next_victim_rather_than_the_small_entries() {
        let c = cache(100 * 10 * KIB + MIB, MIB);
        for i in 0..100u8 {
            c.insert(&format!("small{i}"), blob(10 * KIB, i));
        }

        c.insert("big", blob(MIB, 0xff));
        assert!(c.get("big").is_some());
        c.insert("small100", blob(10 * KIB, 1));

        assert!(c.get("big").is_none(), "the big entry left first");
        for i in 0..100u8 {
            assert!(c.get(&format!("small{i}")).is_some(), "small{i} stayed");
        }
    }

    #[test]
    fn a_big_entry_that_earns_its_hits_outranks_a_small_one() {
        let c = cache(MIB + 10 * KIB, MIB);
        c.insert("small", blob(10 * KIB, 1));
        c.insert("big", blob(MIB, 2));
        for _ in 0..200 {
            c.get("big");
        }

        c.insert("small2", blob(10 * KIB, 3));

        assert!(c.get("big").is_some(), "two hundred hits keep a big entry");
        assert!(
            c.get("small").is_none(),
            "the once-hit small entry was the victim"
        );
    }

    #[test]
    fn the_rising_floor_ages_out_an_entry_that_was_hot_long_ago() {
        let c = cache(30 * KIB, MIB);
        c.insert("old", blob(10 * KIB, 1));
        for _ in 0..1000 {
            c.get("old");
        }

        // The floor rises to the VICTIM's priority, and with three residents the
        // victim is two generations stale, so it climbs once per two evictions:
        // ageing out an entry with n hits takes about 2n evictions, not n.
        for i in 0..2500u32 {
            c.insert(&format!("churn{i}"), blob(10 * KIB, 2));
        }

        assert!(
            c.get("old").is_none(),
            "a thousand hits do not pin an entry forever"
        );
    }

    #[test]
    fn an_insert_the_cache_does_not_admit_drops_the_old_entry() {
        let c = cache(4 * MIB, MIB);
        c.insert("h", blob(KIB, 1));
        c.insert("h", blob(2 * MIB, 2));
        assert!(c.get("h").is_none());
        assert_eq!(c.stats().entries, 0);
    }

    #[test]
    fn a_hit_raises_the_priority_by_hits_over_size() {
        let c = cache(MIB, MIB);
        c.insert("h", blob(4 * KIB, 1));
        let before = c.stats();
        assert_eq!(before.hits, 0);
        c.get("h");
        assert_eq!(c.stats().hits, 1);
    }

    #[tokio::test]
    async fn get_or_load_runs_the_loader_once_for_concurrent_callers() {
        let c = Arc::new(cache(MIB, MIB));
        let loads = Arc::new(AtomicUsize::new(0));
        let tasks = (0..10).map(|_| {
            let c = Arc::clone(&c);
            let loads = Arc::clone(&loads);
            async move {
                c.get_or_load("h", async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    loads.fetch_add(1, Ordering::SeqCst);
                    Ok(Bytes::from_static(b"payload"))
                })
                .await
                .unwrap()
            }
        });

        let results = futures::future::join_all(tasks).await;

        assert_eq!(
            loads.load(Ordering::SeqCst),
            1,
            "one read answers every caller"
        );
        assert!(results.iter().all(|b| b.as_ref() == b"payload"));
        assert_eq!(c.stats().entries, 1);
    }

    #[tokio::test]
    async fn a_failed_load_is_not_cached() {
        let c = cache(MIB, MIB);
        let err = c
            .get_or_load("h", async { Err(anyhow::anyhow!("storage down")) })
            .await;
        assert!(err.is_err());
        assert!(c.get("h").is_none());

        let ok = c
            .get_or_load("h", async { Ok(Bytes::from_static(b"later")) })
            .await
            .unwrap();
        assert_eq!(ok.as_ref(), b"later");
    }

    #[test]
    fn a_disabled_cache_is_a_no_op() {
        let c = HotNarCache::disabled();
        c.insert("h", blob(KIB, 1));
        assert!(c.get("h").is_none());
        assert_eq!(c.stats().entries, 0);
    }
}
