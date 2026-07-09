//! Кеш хешів (T-062): in-memory + helpers для stage2/3.
//!
//! Persistent — `index-sqlite` schema v3 (`file_hash_cache`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use trashradar_domain::candidate::{ByteSize, FsTimestamp};
use trashradar_domain::duplicates::{
    normalize_hash_cache_path, ContentHash, FileHashCacheEntry, PartialHash,
};
use trashradar_domain::error::CoreError;

use crate::ports::{hash_cache_lookup, HashCache, Hasher};
use crate::workers::CancellationToken;

/// In-memory HashCache (тести + hot-session без SQLite).
#[derive(Debug, Default)]
pub struct MemoryHashCache {
    map: Mutex<HashMap<String, FileHashCacheEntry>>,
}

impl MemoryHashCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.map.lock().expect("hash cache mutex").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl HashCache for MemoryHashCache {
    fn get_entry(&self, path: &str) -> Result<Option<FileHashCacheEntry>, CoreError> {
        let key = normalize_hash_cache_path(path);
        Ok(self
            .map
            .lock()
            .expect("hash cache mutex")
            .get(&key)
            .cloned())
    }

    fn put_entry(&self, entry: &FileHashCacheEntry) -> Result<(), CoreError> {
        let mut map = self.map.lock().expect("hash cache mutex");
        let key = entry.path_key.clone();
        if let Some(existing) = map.get_mut(&key) {
            // Merge: оновити validity + заповнити відсутні/нові хеші.
            existing.size = entry.size;
            existing.modified_at = entry.modified_at;
            if entry.partial.is_some() {
                existing.partial = entry.partial;
            }
            if entry.content.is_some() {
                existing.content = entry.content;
            }
        } else {
            map.insert(key, entry.clone());
        }
        Ok(())
    }
}

/// Обгортка Hasher з лічильником реальних I/O-викликів (DoD T-062).
#[derive(Debug)]
pub struct CountingHasher<H: Hasher> {
    pub inner: H,
    pub partial_disk_reads: AtomicU64,
    pub full_disk_reads: AtomicU64,
}

impl<H: Hasher> CountingHasher<H> {
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            partial_disk_reads: AtomicU64::new(0),
            full_disk_reads: AtomicU64::new(0),
        }
    }

    pub fn partial_reads(&self) -> u64 {
        self.partial_disk_reads.load(Ordering::Relaxed)
    }

    pub fn full_reads(&self) -> u64 {
        self.full_disk_reads.load(Ordering::Relaxed)
    }

    pub fn reset_counts(&self) {
        self.partial_disk_reads.store(0, Ordering::Relaxed);
        self.full_disk_reads.store(0, Ordering::Relaxed);
    }
}

impl<H: Hasher> Hasher for CountingHasher<H> {
    fn partial_hash(&self, path: &str, size: ByteSize) -> Result<PartialHash, CoreError> {
        self.partial_disk_reads.fetch_add(1, Ordering::Relaxed);
        self.inner.partial_hash(path, size)
    }

    fn full_hash(
        &self,
        path: &str,
        size: ByteSize,
        cancel: &CancellationToken,
    ) -> Result<ContentHash, CoreError> {
        self.full_disk_reads.fetch_add(1, Ordering::Relaxed);
        self.inner.full_hash(path, size, cancel)
    }
}

/// Зберегти partial у кеш (merge).
pub fn cache_store_partial(
    cache: &dyn HashCache,
    path: &str,
    size: ByteSize,
    modified_at: Option<FsTimestamp>,
    partial: PartialHash,
) -> Result<(), CoreError> {
    let mut entry = hash_cache_lookup(cache, path, size, modified_at)?
        .unwrap_or_else(|| FileHashCacheEntry::new(path, size, modified_at));
    // Якщо validity інша — новий запис.
    if !entry.is_valid_for(size, modified_at) {
        entry = FileHashCacheEntry::new(path, size, modified_at);
    }
    entry.partial = Some(partial);
    cache.put_entry(&entry)
}

/// Зберегти full content hash у кеш (merge).
pub fn cache_store_content(
    cache: &dyn HashCache,
    path: &str,
    size: ByteSize,
    modified_at: Option<FsTimestamp>,
    content: ContentHash,
) -> Result<(), CoreError> {
    let mut entry = hash_cache_lookup(cache, path, size, modified_at)?
        .unwrap_or_else(|| FileHashCacheEntry::new(path, size, modified_at));
    if !entry.is_valid_for(size, modified_at) {
        entry = FileHashCacheEntry::new(path, size, modified_at);
    }
    entry.content = Some(content);
    cache.put_entry(&entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trashradar_domain::duplicates::PartialHash;

    fn ph(b: u8) -> PartialHash {
        let mut a = [0u8; 32];
        a[0] = b;
        PartialHash(a)
    }

    #[test]
    fn memory_cache_roundtrip_and_merge() {
        let cache = MemoryHashCache::new();
        let path = r"C:\data\x.bin";
        let size = ByteSize(42);
        let mtime = Some(FsTimestamp(100));
        cache_store_partial(&cache, path, size, mtime, ph(1)).unwrap();
        let e = hash_cache_lookup(&cache, path, size, mtime)
            .unwrap()
            .unwrap();
        assert_eq!(e.partial, Some(ph(1)));
        assert!(e.content.is_none());

        cache_store_content(&cache, path, size, mtime, ContentHash([9u8; 32])).unwrap();
        let e2 = hash_cache_lookup(&cache, path, size, mtime)
            .unwrap()
            .unwrap();
        assert_eq!(e2.partial, Some(ph(1)));
        assert_eq!(e2.content, Some(ContentHash([9u8; 32])));
    }

    #[test]
    fn stale_mtime_misses() {
        let cache = MemoryHashCache::new();
        let path = r"D:\a";
        cache_store_partial(&cache, path, ByteSize(1), Some(FsTimestamp(1)), ph(2)).unwrap();
        assert!(
            hash_cache_lookup(&cache, path, ByteSize(1), Some(FsTimestamp(2)))
                .unwrap()
                .is_none()
        );
    }
}
