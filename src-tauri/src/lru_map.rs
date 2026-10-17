//! A map that holds at most `cap` entries and, when full, drops the one used
//! longest ago.
//!
//! The process-wide memories in front of Git and the ledger used to empty
//! themselves when they filled: one new repository forgot every other
//! repository's answer, so a session with more families open than the cap paid
//! the cold cost on every read, and a test's insert wiped a sibling test's warm
//! entry between its cold and warm read. Evicting only the least recently used
//! entry keeps whatever is still in use.
//!
//! Recency is a monotonic counter stamped on every hit and insert. Eviction is
//! a scan, paid only when a *new* key arrives at a full map; the caps here are
//! hundreds, and a hit never scans.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;

pub struct LruMap<K, V> {
    cap: usize,
    clock: u64,
    entries: HashMap<K, (u64, V)>,
}

impl<K: Eq + Hash + Clone, V> LruMap<K, V> {
    /// An empty map holding at most `cap` entries (at least one).
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            clock: 0,
            entries: HashMap::new(),
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Whether `key` is held. Does not count as a use.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.entries.contains_key(key)
    }

    /// The value for `key`, counted as a use.
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.get_mut(key).map(|value| &*value)
    }

    /// The value for `key`, mutably, counted as a use.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let stamp = self.tick();
        self.entries.get_mut(key).map(|(used, value)| {
            *used = stamp;
            value
        })
    }

    /// Stores `value` under `key` as the most recently used entry. A new key
    /// arriving at a full map evicts exactly the least recently used one.
    pub fn insert(&mut self, key: K, value: V) {
        self.make_room_for(&key);
        let stamp = self.tick();
        self.entries.insert(key, (stamp, value));
    }

    /// The value for `key`, created by `make` if absent, counted as a use. A
    /// new key arriving at a full map evicts exactly the least recently used
    /// one.
    pub fn get_or_insert_with(&mut self, key: K, make: impl FnOnce() -> V) -> &mut V {
        self.make_room_for(&key);
        let stamp = self.tick();
        let (used, value) = self.entries.entry(key).or_insert_with(|| (stamp, make()));
        *used = stamp;
        value
    }

    fn make_room_for(&mut self, key: &K) {
        if self.entries.len() < self.cap || self.entries.contains_key(key) {
            return;
        }
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, (used, _))| *used)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            self.entries.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(map: &LruMap<u32, u32>) -> Vec<u32> {
        let mut keys: Vec<u32> = map.entries.keys().copied().collect();
        keys.sort_unstable();
        keys
    }

    /// The failure this replaces: the map emptied itself at the cap, so one new
    /// key forgot all of the others.
    #[test]
    fn a_full_map_drops_exactly_the_least_recently_used_entry() {
        let mut map = LruMap::new(4);
        for key in 0..4 {
            map.insert(key, key);
        }
        map.insert(4, 4);
        assert_eq!(held(&map), [1, 2, 3, 4], "only key 0 (the oldest) goes");
        assert_eq!(map.len(), 4);
    }

    #[test]
    fn a_hit_refreshes_recency() {
        let mut map = LruMap::new(4);
        for key in 0..4 {
            map.insert(key, key);
        }
        assert_eq!(map.get(&0), Some(&0), "touch the oldest");
        map.insert(4, 4);
        assert_eq!(held(&map), [0, 2, 3, 4], "key 1 is now the oldest");
    }

    #[test]
    fn a_mutable_hit_and_a_replace_both_refresh_recency() {
        let mut map = LruMap::new(3);
        for key in 0..3 {
            map.insert(key, key);
        }
        *map.get_mut(&0).expect("held") += 10;
        map.insert(1, 11);
        map.insert(3, 3);
        assert_eq!(held(&map), [0, 1, 3], "key 2 was the least recently used");
        assert_eq!(map.get(&0), Some(&10));
    }

    #[test]
    fn replacing_a_held_key_never_evicts() {
        let mut map = LruMap::new(2);
        map.insert(0, 0);
        map.insert(1, 1);
        map.insert(1, 9);
        assert_eq!(held(&map), [0, 1]);
        assert_eq!(map.get(&1), Some(&9));
    }

    #[test]
    fn get_or_insert_with_evicts_one_and_keeps_an_existing_value() {
        let mut map = LruMap::new(2);
        *map.get_or_insert_with(0, || 0) += 1;
        *map.get_or_insert_with(1, || 0) += 1;
        *map.get_or_insert_with(0, || 100) += 1;
        assert_eq!(map.get(&0), Some(&2), "an existing value is not rebuilt");
        *map.get_or_insert_with(2, || 7) += 1;
        assert_eq!(held(&map), [0, 2], "key 1 was the least recently used");
        assert_eq!(map.get(&2), Some(&8));
    }

    #[test]
    fn contains_key_is_not_a_use_and_a_zero_cap_still_holds_one() {
        let mut map = LruMap::new(2);
        map.insert(0, 0);
        map.insert(1, 1);
        assert!(map.contains_key(&0));
        map.insert(2, 2);
        assert_eq!(held(&map), [1, 2], "peeking at key 0 did not save it");

        let mut tiny = LruMap::new(0);
        tiny.insert(1, 1);
        tiny.insert(2, 2);
        assert_eq!(held(&tiny), [2]);
    }
}
