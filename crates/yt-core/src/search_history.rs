//! Local search history: the user's recent queries (newest first, deduplicated, at most
//! [`MAX_ENTRIES`]), kept in `search_history.json`. This is the app's own list; clearing the
//! signed-in account's YouTube search history is `Session::clear_search_history` in `actions`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "search_history.json";
pub const MAX_ENTRIES: usize = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchHistory {
    /// Newest first.
    #[serde(default)]
    pub queries: Vec<String>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

fn normalize(q: &str) -> String {
    q.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl SearchHistory {
    pub fn load() -> Self {
        Self::load_from(&crate::paths::data_file(FILE_NAME))
    }

    pub fn load_from(path: &Path) -> Self {
        let mut h: SearchHistory = crate::fsutil::load_json_or_default(path);
        h.queries.truncate(MAX_ENTRIES);
        h.path = Some(path.to_path_buf());
        h
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path.clone().unwrap_or_else(|| crate::paths::data_file(FILE_NAME));
        crate::fsutil::save_json(&path, self)
    }

    /// Puts `query` at the top (moving it if already present, compared case-insensitively and
    /// ignoring extra spaces). Blank queries are ignored. Returns true if the list changed.
    pub fn add(&mut self, query: &str) -> bool {
        let q = normalize(query);
        if q.is_empty() {
            return false;
        }
        if self.queries.first().is_some_and(|f| *f == q) {
            return false;
        }
        let key = q.to_lowercase();
        self.queries.retain(|e| e.to_lowercase() != key);
        self.queries.insert(0, q);
        self.queries.truncate(MAX_ENTRIES);
        true
    }

    pub fn remove(&mut self, query: &str) -> bool {
        let key = normalize(query).to_lowercase();
        let n = self.queries.len();
        self.queries.retain(|e| e.to_lowercase() != key);
        n != self.queries.len()
    }

    pub fn clear(&mut self) {
        self.queries.clear();
    }

    pub fn list(&self) -> &[String] {
        &self.queries
    }

    /// Entries that start with (or, after those, contain) `prefix`, case-insensitively, for
    /// suggestions while typing. Empty prefix -> the newest `limit` entries.
    pub fn matching(&self, prefix: &str, limit: usize) -> Vec<String> {
        let p = normalize(prefix).to_lowercase();
        let starts = self.queries.iter().filter(|q| q.to_lowercase().starts_with(&p));
        let contains = self.queries.iter().filter(|q| {
            let l = q.to_lowercase();
            !l.starts_with(&p) && l.contains(&p)
        });
        starts.chain(contains).take(limit).cloned().collect()
    }
}

/// Load, add, save.
pub fn record(query: &str) -> Result<()> {
    let mut h = SearchHistory::load();
    if h.add(query) {
        h.save()?;
    }
    Ok(())
}

/// Load, remove, save.
pub fn remove(query: &str) -> Result<bool> {
    let mut h = SearchHistory::load();
    let r = h.remove(query);
    if r {
        h.save()?;
    }
    Ok(r)
}

/// Load, clear, save.
pub fn clear() -> Result<()> {
    let mut h = SearchHistory::load();
    h.clear();
    h.save()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_dedupe_limit() {
        let mut h = SearchHistory::default();
        assert!(h.add("cats"));
        assert!(h.add("  dogs   and  more "));
        assert!(!h.add("   "));
        assert!(h.add("CATS"));
        assert_eq!(h.list(), ["CATS", "dogs and more"]);
        assert!(!h.add("CATS"));
        for i in 0..60 {
            h.add(&format!("q{i}"));
        }
        assert_eq!(h.list().len(), MAX_ENTRIES);
        assert_eq!(h.list()[0], "q59");
        assert!(h.remove("Q59"));
        assert!(!h.remove("q59"));
        assert_eq!(h.list()[0], "q58");
        h.clear();
        assert!(h.list().is_empty());
    }

    #[test]
    fn matching_order() {
        let mut h = SearchHistory::default();
        for q in ["rust tv", "trust", "rust lang", "python"] {
            h.add(q);
        }
        assert_eq!(h.matching("rust", 10), ["rust lang", "rust tv", "trust"]);
        assert_eq!(h.matching("", 2), ["python", "rust lang"]);
    }

    #[test]
    fn persist() {
        let d = crate::fsutil::temp_dir("search");
        let p = d.join(FILE_NAME);
        let mut h = SearchHistory::load_from(&p);
        h.add("a");
        h.add("b");
        h.save().unwrap();
        assert_eq!(SearchHistory::load_from(&p).list(), ["b", "a"]);
        std::fs::remove_dir_all(d).unwrap();
    }
}
