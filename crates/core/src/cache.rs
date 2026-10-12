//! Single-threaded weighted LRU. Lookups touch metadata without allocating or scanning.
use std::borrow::Borrow;
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::Hash;

struct Entry<V> {
    value: V,
    bytes: usize,
    used: Cell<u64>,
}

pub struct BudgetCache<K, V> {
    entries: HashMap<K, Entry<V>>,
    max_entries: usize,
    max_bytes: usize,
    bytes: usize,
    clock: Cell<u64>,
}

impl<K: Eq + Hash + Clone, V> BudgetCache<K, V> {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        assert!(max_entries > 0 && max_bytes > 0);
        Self {
            entries: HashMap::new(),
            max_entries,
            max_bytes,
            bytes: 0,
            clock: Cell::new(0),
        }
    }

    fn touch(&self) -> u64 {
        let next = self.clock.get().wrapping_add(1);
        self.clock.set(next);
        next
    }

    pub fn get<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        let entry = self.entries.get(key)?;
        entry.used.set(self.touch());
        Some(&entry.value)
    }

    /// Preserve one oversized resource rather than upload/decode it every frame. This is
    /// relevant to screen-sized crops; all other entries are evicted in that case.
    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.remove(&key);
        let used = self.touch();
        self.bytes = self.bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                used: Cell::new(used),
            },
        );
        while self.entries.len() > self.max_entries
            || (self.bytes > self.max_bytes && self.entries.len() > 1)
        {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used.get())
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest {
                self.remove(&key);
            } else {
                break;
            }
        }
    }

    pub fn remove<Q: Eq + Hash + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let entry = self.entries.remove(key)?;
        self.bytes -= entry.bytes;
        Some(entry.value)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        self.entries.retain(|key, entry| {
            if keep(key, &entry.value) {
                true
            } else {
                self.bytes -= entry.bytes;
                false
            }
        });
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn byte_pressure_evicts_lru_and_visible_references_survive() {
        let mut c = BudgetCache::new(10, 8);
        let visible = Rc::new(vec![1, 2, 3, 4]);
        c.insert("a".to_string(), visible.clone(), 4);
        c.insert("b".to_string(), Rc::new(vec![5; 4]), 4);
        c.get("a");
        c.insert("c".to_string(), Rc::new(vec![6; 4]), 4);
        assert!(c.get("b").is_none());
        c.insert("d".to_string(), Rc::new(vec![7; 4]), 4);
        assert!(c.get("a").is_none());
        assert_eq!(visible.as_slice(), &[1, 2, 3, 4]);
        assert_eq!((c.len(), c.bytes()), (2, 8));
    }

    #[test]
    fn failure_entries_are_bounded_even_without_pixel_weight() {
        let mut c = BudgetCache::new(2, 100);
        for key in 0..20 {
            c.insert(key, None::<u8>, 0);
        }
        assert_eq!((c.len(), c.bytes()), (2, 0));
        assert_eq!(c.get(&19), Some(&None));
    }

    #[test]
    fn replacement_invalidation_and_clear_account_for_bytes() {
        let mut c = BudgetCache::new(10, 100);
        c.insert(1, "old", 30);
        c.insert(1, "new", 4);
        c.insert(2, "other", 5);
        assert_eq!(c.bytes(), 9);
        c.retain(|k, _| *k == 1);
        assert_eq!(c.bytes(), 4);
        assert_eq!(c.remove(&1), Some("new"));
        assert_eq!(c.bytes(), 0);
        c.insert(3, "last", 10);
        c.clear();
        assert_eq!((c.len(), c.bytes()), (0, 0));
    }

    #[test]
    fn a_single_oversized_crop_does_not_cause_upload_churn() {
        let mut c = BudgetCache::new(10, 8);
        c.insert(1, "small", 4);
        c.insert(2, "large", 20);
        assert_eq!((c.len(), c.bytes()), (1, 20));
        assert_eq!(c.get(&2), Some(&"large"));
        c.insert(3, "small", 4);
        assert_eq!((c.len(), c.bytes()), (1, 4));
    }
}
