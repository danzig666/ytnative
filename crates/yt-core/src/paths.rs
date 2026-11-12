//! Where yt-core keeps its files (sign-in token, settings). The PC CLI uses the current directory;
//! the TV app sets a persistent directory at boot.

use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Sets the data directory once, at startup. Later calls are ignored.
pub fn set_data_dir(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let _ = DATA_DIR.set(dir);
}

pub fn data_file(name: &str) -> PathBuf {
    DATA_DIR.get().map(|d| d.join(name)).unwrap_or_else(|| PathBuf::from(name))
}
