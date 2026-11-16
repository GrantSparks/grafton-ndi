//! A bounded map whose inserts and removals reuse storage reserved at creation.

use std::{
    collections::{hash_map, HashMap},
    hash::Hash,
};

#[cfg(any(
    test,
    ndi_has_advanced_receive_allocators,
    ndi_has_advanced_send_completion
))]
use std::collections::TryReserveError;

/// Both the table and its rebuild workspace are reserved before use. Removal
/// tombstones can reduce a `HashMap`'s insertion capacity; draining it into the
/// workspace and back clears those tombstones without allocating. Keys must
/// retain their hash and equality while stored, as required by `HashMap`.
pub(super) struct ReservedMap<K, V> {
    entries: HashMap<K, V>,
    scratch: Vec<(K, V)>,
}

impl<K, V> Default for ReservedMap<K, V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            scratch: Vec::new(),
        }
    }
}

impl<K: Eq + Hash, V> ReservedMap<K, V> {
    #[cfg(any(
        test,
        ndi_has_advanced_receive_allocators,
        ndi_has_advanced_send_completion
    ))]
    pub(super) fn try_with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        let mut map = Self::default();
        map.entries.try_reserve(capacity)?;
        map.scratch.try_reserve_exact(capacity)?;
        Ok(map)
    }

    /// Return a duplicate or an item beyond the reserved bound unchanged.
    /// Neither accepted nor refused insertion allocates table storage.
    #[cfg(any(test, ndi_has_advanced_receive_allocators))]
    pub(super) fn insert_new(&mut self, key: K, value: V) -> Result<(), (K, V)> {
        if self.entries.contains_key(&key) || !self.prepare_insert() {
            return Err((key, value));
        }
        self.entries.insert(key, value);
        Ok(())
    }

    /// Build the stored value only after admission. Queued descriptor owners
    /// use this to keep their move-only initialization key on a refused insert.
    pub(super) fn insert_new_with<T>(
        &mut self,
        key: K,
        input: T,
        value: impl FnOnce(T) -> V,
    ) -> Result<(), T> {
        if !self.prepare_insert() {
            return Err(input);
        }
        match self.entries.entry(key) {
            hash_map::Entry::Occupied(_) => Err(input),
            hash_map::Entry::Vacant(entry) => {
                entry.insert(value(input));
                Ok(())
            }
        }
    }

    fn prepare_insert(&mut self) -> bool {
        // A Vec of zero-sized pairs reports usize::MAX capacity even when no
        // room was requested. An empty HashMap still records that zero bound.
        if self.entries.capacity() == 0 || self.entries.len() == self.scratch.capacity() {
            return false;
        }
        if self.entries.capacity() == self.entries.len() {
            self.rebuild();
        }
        true
    }

    fn rebuild(&mut self) {
        debug_assert!(self.scratch.is_empty());
        debug_assert!(self.scratch.capacity() >= self.entries.len());
        self.scratch.extend(self.entries.drain());
        debug_assert!(self.entries.capacity() > self.scratch.len());
        for (key, value) in self.scratch.drain(..) {
            self.entries.insert(key, value);
        }
        debug_assert!(self.entries.capacity() > self.entries.len());
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(any(
        test,
        ndi_has_advanced_receive_allocators,
        ndi_has_advanced_send_completion
    ))]
    pub(super) fn remove(&mut self, key: &K) -> Option<V> {
        self.entries.remove(key)
    }

    pub(super) fn drain(&mut self) -> hash_map::Drain<'_, K, V> {
        self.entries.drain()
    }

    #[cfg(test)]
    pub(super) fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key)
    }

    #[cfg(test)]
    pub(super) fn iter(&self) -> hash_map::Iter<'_, K, V> {
        self.entries.iter()
    }

    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertion_and_removal_reuse_reserved_storage_across_churn_and_rebuilds() {
        for capacity in [2, 4, 7, 8, 14, 16, 28, 56, 112] {
            let mut map = ReservedMap::try_with_capacity(capacity).unwrap();
            let mut retained: Vec<_> = (0..capacity - 1).collect();
            for &key in &retained {
                map.insert_new(key, key + 1).unwrap();
            }
            let reserved = (map.capacity(), map.scratch.capacity());
            let changes = if cfg!(miri) { 128 } else { 2048 };
            let mut forced_rebuilds = 0;
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                for change in 0..changes {
                    let key = capacity + change;
                    map.insert_new(key, key + 1).unwrap();
                    let old = std::mem::replace(&mut retained[change % (capacity - 1)], key);
                    assert_eq!(map.remove(&old), Some(old + 1));
                    assert_eq!(map.len(), capacity - 1);
                    // Small tables may erase a removal without a tombstone.
                    // Force several rebuilds at every capacity as well as
                    // exercising automatic rebuilding during ordinary churn.
                    if change % 32 == 0 {
                        map.rebuild();
                        forced_rebuilds += 1;
                    }
                }
            });
            assert_eq!(allocations, 0, "capacity {capacity}");
            assert!(forced_rebuilds >= 4);
            assert_eq!(map.scratch.capacity(), reserved.1);
            assert!(map.scratch.is_empty());
            map.rebuild();
            assert_eq!(map.capacity(), reserved.0);
            for key in retained {
                assert_eq!(map.get(&key), Some(&(key + 1)));
            }
            assert!(map.iter().all(|(key, value)| *value == key + 1));
        }
    }

    #[derive(Debug)]
    struct Key {
        number: usize,
        label: &'static str,
    }

    impl PartialEq for Key {
        fn eq(&self, other: &Self) -> bool {
            self.number == other.number
        }
    }

    impl Eq for Key {}

    impl Hash for Key {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.number.hash(state);
        }
    }

    #[test]
    fn a_duplicate_returns_the_original_key_and_value_without_replacing_storage() {
        let mut map = ReservedMap::try_with_capacity(2).unwrap();
        map.insert_new(
            Key {
                number: 1,
                label: "stored",
            },
            11,
        )
        .unwrap();
        let duplicate = Key {
            number: 1,
            label: "returned",
        };
        let ((key, value), allocations) =
            crate::test_alloc::allocation_count(|| map.insert_new(duplicate, 22).unwrap_err());
        assert_eq!(allocations, 0);
        assert_eq!((key.label, value), ("returned", 22));
        let (key, value) = map.iter().next().unwrap();
        assert_eq!((key.label, *value), ("stored", 11));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn a_refused_insertion_never_initializes_the_value() {
        let mut map = ReservedMap::try_with_capacity(2).unwrap();
        map.insert_new(1, 11).unwrap();
        assert_eq!(
            map.insert_new_with(1, 22, |_| panic!("a duplicate must not build a value")),
            Err(22)
        );
        map.insert_new(2, 33).unwrap();
        assert_eq!(
            map.insert_new_with(3, 44, |_| panic!("a full map must not build a value")),
            Err(44)
        );
    }

    #[test]
    fn the_reserved_bound_refuses_new_items_without_allocating() {
        let mut empty = ReservedMap::<(), ()>::try_with_capacity(0).unwrap();
        assert_eq!(empty.insert_new((), ()), Err(((), ())));
        for capacity in [0, 1, 7] {
            let mut map = ReservedMap::try_with_capacity(capacity).unwrap();
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                for key in 0..capacity {
                    map.insert_new(key, key + 1).unwrap();
                }
                assert_eq!(map.insert_new(capacity, 99), Err((capacity, 99)));
            });
            assert_eq!(allocations, 0);
            assert_eq!(map.len(), capacity);
        }
    }

    #[test]
    fn draining_preserves_pairs_and_keeps_the_storage_for_reuse() {
        let mut map = ReservedMap::try_with_capacity(2).unwrap();
        map.insert_new(1, 10).unwrap();
        map.insert_new(2, 20).unwrap();
        let mut pairs: Vec<_> = map.drain().collect();
        pairs.sort_unstable();
        assert_eq!(pairs, [(1, 10), (2, 20)]);
        assert!(map.is_empty());
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            map.insert_new(3, 30).unwrap();
            map.insert_new(4, 40).unwrap();
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn an_impossible_reservation_is_fallible() {
        assert!(ReservedMap::<usize, usize>::try_with_capacity(usize::MAX).is_err());
    }
}
