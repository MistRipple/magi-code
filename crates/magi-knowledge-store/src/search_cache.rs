use std::collections::{HashMap, VecDeque};

pub struct SearchCacheConfig {
    pub max_size: usize,
    pub ttl_ms: u64,
}

impl Default for SearchCacheConfig {
    fn default() -> Self {
        Self {
            max_size: 20,
            ttl_ms: 60_000,
        }
    }
}

struct CacheEntry<T> {
    value: T,
    timestamp: u64,
    file_paths: Vec<String>,
}

pub struct SearchCacheStats {
    pub size: usize,
    pub max_size: usize,
    pub hit_count: u64,
    pub miss_count: u64,
    pub hit_rate: f64,
}

pub struct SearchCache<T: Clone> {
    entries: HashMap<String, CacheEntry<T>>,
    order: VecDeque<String>,
    config: SearchCacheConfig,
    hit_count: u64,
    miss_count: u64,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl<T: Clone> SearchCache<T> {
    pub fn new(config: SearchCacheConfig) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            config,
            hit_count: 0,
            miss_count: 0,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(SearchCacheConfig::default())
    }

    pub fn get(&mut self, query: &str) -> Option<T> {
        let key = Self::normalize_key(query);
        let entry = match self.entries.get(&key) {
            Some(e) => e,
            None => {
                self.miss_count += 1;
                return None;
            }
        };

        if now_millis() - entry.timestamp > self.config.ttl_ms {
            self.entries.remove(&key);
            self.order.retain(|k| k != &key);
            self.miss_count += 1;
            return None;
        }

        let value = entry.value.clone();
        self.order.retain(|k| k != &key);
        self.order.push_back(key);
        self.hit_count += 1;
        Some(value)
    }

    pub fn set(&mut self, query: &str, value: T, file_paths: Vec<String>) {
        let key = Self::normalize_key(query);

        if self.entries.contains_key(&key) {
            self.entries.remove(&key);
            self.order.retain(|k| k != &key);
        }

        while self.entries.len() >= self.config.max_size {
            if let Some(oldest_key) = self.order.pop_front() {
                self.entries.remove(&oldest_key);
            } else {
                break;
            }
        }

        self.entries.insert(
            key.clone(),
            CacheEntry {
                value,
                timestamp: now_millis(),
                file_paths,
            },
        );
        self.order.push_back(key);
    }

    pub fn invalidate_all(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    pub fn invalidate_by_file(&mut self, file_path: &str) {
        let keys_to_remove: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.file_paths.iter().any(|p| p == file_path))
            .map(|(k, _)| k.clone())
            .collect();
        for key in &keys_to_remove {
            self.entries.remove(key);
        }
        self.order.retain(|k| !keys_to_remove.contains(k));
    }

    pub fn get_stats(&self) -> SearchCacheStats {
        let total = self.hit_count + self.miss_count;
        SearchCacheStats {
            size: self.entries.len(),
            max_size: self.config.max_size,
            hit_count: self.hit_count,
            miss_count: self.miss_count,
            hit_rate: if total > 0 {
                self.hit_count as f64 / total as f64
            } else {
                0.0
            },
        }
    }

    fn normalize_key(query: &str) -> String {
        let normalized = query.trim().to_lowercase();
        let mut result = String::with_capacity(normalized.len());
        let mut prev_space = false;
        for c in normalized.chars() {
            if c.is_whitespace() {
                if !prev_space {
                    result.push(' ');
                    prev_space = true;
                }
            } else {
                result.push(c);
                prev_space = false;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_get_set() {
        let mut cache = SearchCache::<Vec<String>>::with_defaults();
        cache.set("hello world", vec!["a.rs".into()], vec!["a.rs".into()]);
        assert!(cache.get("hello world").is_some());
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn test_lru_eviction() {
        let mut cache = SearchCache::<i32>::new(SearchCacheConfig {
            max_size: 3,
            ttl_ms: 60_000,
        });
        cache.set("a", 1, vec![]);
        cache.set("b", 2, vec![]);
        cache.set("c", 3, vec![]);
        cache.set("d", 4, vec![]);
        assert!(cache.get("a").is_none());
        assert_eq!(cache.get("b"), Some(2));
    }

    #[test]
    fn test_invalidate_by_file() {
        let mut cache = SearchCache::<i32>::with_defaults();
        cache.set("q1", 1, vec!["a.rs".into(), "b.rs".into()]);
        cache.set("q2", 2, vec!["c.rs".into()]);
        cache.invalidate_by_file("a.rs");
        assert!(cache.get("q1").is_none());
        assert_eq!(cache.get("q2"), Some(2));
    }

    #[test]
    fn test_normalize_key() {
        let mut cache = SearchCache::<i32>::with_defaults();
        cache.set("  Hello   World  ", 42, vec![]);
        assert_eq!(cache.get("hello world"), Some(42));
    }

    #[test]
    fn test_stats() {
        let mut cache = SearchCache::<i32>::with_defaults();
        cache.set("a", 1, vec![]);
        cache.get("a");
        cache.get("b");
        let stats = cache.get_stats();
        assert_eq!(stats.hit_count, 1);
        assert_eq!(stats.miss_count, 1);
        assert!((stats.hit_rate - 0.5).abs() < 0.01);
    }
}
