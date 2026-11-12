//! A tiny thread-safe in-memory cache with a time-to-live per entry.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct TtlCache<K, V> {
    ttl: Duration,
    capacity: usize,
    map: Mutex<HashMap<K, (Instant, V)>>,
}

impl<K: Eq + Hash + Clone, V: Clone> TtlCache<K, V> {
    /// `capacity` bounds memory: when full, expired entries are dropped first, then the oldest one.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        TtlCache { ttl, capacity: capacity.max(1), map: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let mut map = self.map.lock().unwrap();
        match map.get(key) {
            Some((at, v)) if at.elapsed() < self.ttl => Some(v.clone()),
            Some(_) => {
                map.remove(key);
                None
            }
            None => None,
        }
    }

    pub fn insert(&self, key: K, value: V) {
        let mut map = self.map.lock().unwrap();
        if map.len() >= self.capacity && !map.contains_key(&key) {
            let ttl = self.ttl;
            map.retain(|_, (at, _)| at.elapsed() < ttl);
            if map.len() >= self.capacity {
                if let Some(oldest) = map.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) {
                    map.remove(&oldest);
                }
            }
        }
        map.insert(key, (Instant::now(), value));
    }

    pub fn remove(&self, key: &K) {
        self.map.lock().unwrap().remove(key);
    }

    pub fn clear(&self) {
        self.map.lock().unwrap().clear();
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Cached value, or compute it with `f` (outside the lock) and cache it on success.
    /// Errors are not cached, so a later call retries.
    pub fn get_or_try_insert_with<E>(&self, key: K, f: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        if let Some(v) = self.get(&key) {
            return Ok(v);
        }
        let v = f()?;
        self.insert(key, v.clone());
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_and_capacity() {
        let c: TtlCache<&str, i32> = TtlCache::new(Duration::from_millis(40), 2);
        c.insert("a", 1);
        assert_eq!(c.get(&"a"), Some(1));
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(c.get(&"a"), None);

        c.insert("a", 1);
        std::thread::sleep(Duration::from_millis(2));
        c.insert("b", 2);
        c.insert("c", 3); // evicts the oldest ("a")
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(&"a"), None);
        assert_eq!(c.get(&"c"), Some(3));
    }

    #[test]
    fn try_insert_caches_only_success() {
        let c: TtlCache<u8, u8> = TtlCache::new(Duration::from_secs(60), 8);
        assert!(c.get_or_try_insert_with(1, || Err::<u8, ()>(())).is_err());
        assert!(c.is_empty());
        assert_eq!(c.get_or_try_insert_with(1, || Ok::<_, ()>(7)), Ok(7));
        assert_eq!(c.get_or_try_insert_with(1, || Ok::<_, ()>(9)), Ok(7));
    }
}
