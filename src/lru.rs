//! A small weight-capped LRU cache.
//!
//! Each entry carries a weight (bytes, for thumbnails). Inserting past the cap
//! evicts least-recently-used entries and hands them back to the caller, so the
//! caller can release resources tied to them (e.g. GPU atlas textures).

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

pub struct WeightedLru<K, V> {
    cap: usize,
    weight: usize,
    tick: u64,
    entries: HashMap<K, Entry<V>>,
    /// tick -> key; the smallest tick is the least recently used.
    order: BTreeMap<u64, K>,
}

struct Entry<V> {
    value: V,
    weight: usize,
    tick: u64,
}

impl<K: Hash + Eq + Clone, V> WeightedLru<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            weight: 0,
            tick: 0,
            entries: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn weight(&self) -> usize {
        self.weight
    }

    #[cfg(test)]
    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Returns the value and marks it most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.tick);
        entry.tick = tick;
        self.order.insert(tick, key.clone());
        Some(&entry.value)
    }

    /// Inserts (or replaces) a value. Returns every value evicted to stay under
    /// the cap, including a replaced value for the same key. An entry heavier
    /// than the whole cap is rejected and returned immediately.
    pub fn insert(&mut self, key: K, value: V, weight: usize) -> Vec<V> {
        let mut evicted = Vec::new();
        if let Some(old) = self.remove(&key) {
            evicted.push(old);
        }
        if weight > self.cap {
            evicted.push(value);
            return evicted;
        }
        while self.weight + weight > self.cap {
            match self.pop_lru() {
                Some(v) => evicted.push(v),
                None => break,
            }
        }
        self.tick += 1;
        self.order.insert(self.tick, key.clone());
        self.entries.insert(
            key,
            Entry {
                value,
                weight,
                tick: self.tick,
            },
        );
        self.weight += weight;
        evicted
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let entry = self.entries.remove(key)?;
        self.order.remove(&entry.tick);
        self.weight -= entry.weight;
        Some(entry.value)
    }

    /// Removes everything, returning the values.
    pub fn drain(&mut self) -> Vec<V> {
        self.order.clear();
        self.weight = 0;
        self.entries.drain().map(|(_, e)| e.value).collect()
    }

    fn pop_lru(&mut self) -> Option<V> {
        let (_, key) = self.order.pop_first()?;
        let entry = self.entries.remove(&key)?;
        self.weight -= entry.weight;
        Some(entry.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_by_weight() {
        let mut lru = WeightedLru::new(100);
        assert!(lru.insert("a", 1, 40).is_empty());
        assert!(lru.insert("b", 2, 40).is_empty());
        // Touch "a" so "b" becomes the LRU entry.
        assert_eq!(lru.get(&"a"), Some(&1));
        let evicted = lru.insert("c", 3, 40);
        assert_eq!(evicted, vec![2]);
        assert!(lru.contains(&"a"));
        assert!(!lru.contains(&"b"));
        assert!(lru.contains(&"c"));
        assert_eq!(lru.weight(), 80);
    }

    #[test]
    fn evicts_multiple_to_fit_heavy_entry() {
        let mut lru = WeightedLru::new(100);
        lru.insert(1, "one", 30);
        lru.insert(2, "two", 30);
        lru.insert(3, "three", 30);
        let evicted = lru.insert(4, "four", 70);
        assert_eq!(evicted, vec!["one", "two"]);
        assert_eq!(lru.len(), 2);
        assert_eq!(lru.weight(), 100);
    }

    #[test]
    fn replacing_key_returns_old_value_and_fixes_weight() {
        let mut lru = WeightedLru::new(100);
        lru.insert("k", 1, 50);
        let evicted = lru.insert("k", 2, 20);
        assert_eq!(evicted, vec![1]);
        assert_eq!(lru.weight(), 20);
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.get(&"k"), Some(&2));
    }

    #[test]
    fn rejects_entry_heavier_than_cap() {
        let mut lru = WeightedLru::new(10);
        lru.insert("small", 1, 5);
        let evicted = lru.insert("huge", 2, 11);
        assert_eq!(evicted, vec![2]);
        assert!(lru.contains(&"small"));
        assert!(!lru.contains(&"huge"));
        assert_eq!(lru.weight(), 5);
    }

    #[test]
    fn never_exceeds_cap_under_churn() {
        let mut lru = WeightedLru::new(1000);
        for i in 0..10_000u32 {
            let w = (i as usize * 37) % 200 + 1;
            lru.insert(i, i, w);
            if i % 3 == 0 {
                lru.get(&(i / 2));
            }
            assert!(lru.weight() <= lru.cap());
        }
        let sum: usize = lru.entries.values().map(|e| e.weight).sum();
        assert_eq!(sum, lru.weight());
        assert_eq!(lru.order.len(), lru.entries.len());
    }

    #[test]
    fn drain_empties_cache() {
        let mut lru = WeightedLru::new(100);
        lru.insert(1, "a", 10);
        lru.insert(2, "b", 10);
        let mut all = lru.drain();
        all.sort();
        assert_eq!(all, vec!["a", "b"]);
        assert_eq!((lru.len(), lru.weight()), (0, 0));
        assert!(lru.insert(3, "c", 100).is_empty());
    }

    #[test]
    fn remove_frees_weight() {
        let mut lru = WeightedLru::new(100);
        lru.insert("a", (), 60);
        assert!(lru.remove(&"a").is_some());
        assert_eq!(lru.weight(), 0);
        assert!(lru.insert("b", (), 100).is_empty());
    }
}
