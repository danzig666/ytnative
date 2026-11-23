//! Videos the viewer marked as watched on this TV ("Mark as watched" in the card menu, as in
//! SmartTube): their cards show a full progress bar and "Hide watched" hides them, whatever
//! YouTube's own history says. Kept in `watched.json`, the newest few thousand.

use crate::memory::LruMap;
use crate::model::Item;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE: &str = "watched.json";
const CAP: usize = 5000;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Watched {
    /// Video id -> Unix seconds it was marked.
    #[serde(default)]
    ids: LruMap<u64>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Watched {
    pub fn load() -> Self {
        Self::load_from(&crate::paths::data_file(FILE))
    }

    pub fn load_from(path: &Path) -> Self {
        let mut w: Watched = std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        w.path = Some(path.to_path_buf());
        w
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(p) = &self.path else { return Ok(()) };
        crate::fsutil::write_atomic(p, serde_json::to_string(self)?.as_bytes())?;
        Ok(())
    }

    pub fn contains(&self, video_id: &str) -> bool {
        self.ids.peek(video_id).is_some()
    }

    /// Marks (`true`) or unmarks the videos.
    pub fn set(&mut self, video_ids: &[String], watched: bool, now: u64) {
        for id in video_ids.iter().filter(|id| !id.is_empty()) {
            if watched {
                self.ids.insert(id, now, CAP);
            } else {
                self.ids.remove(id);
            }
        }
    }

    /// `it` with a full progress bar when it was marked.
    pub fn apply(&self, it: &Item) -> Item {
        let mut it = it.clone();
        if self.contains(&it.id) {
            it.progress = Some(100);
        }
        it
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_survive_a_reload() {
        let p = std::env::temp_dir().join(format!("ytnative-watched-{}.json", std::process::id()));
        let mut w = Watched::load_from(&p);
        w.set(&["a".into(), "b".into()], true, 1);
        w.set(&["b".into()], false, 2);
        w.save().unwrap();
        let w = Watched::load_from(&p);
        assert!(w.contains("a") && !w.contains("b"));
        let it = Item { id: "a".into(), ..Default::default() };
        assert_eq!(w.apply(&it).progress, Some(100));
        let _ = std::fs::remove_file(p);
    }
}
