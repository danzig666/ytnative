//! Small helpers for the app's own JSON files: atomic writes (tmp + rename) and tolerant loads.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// Writes `bytes` to `path` via a sibling temp file and a rename, so a crash or power cut never
/// leaves a half-written file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

/// Serializes `v` as pretty JSON and writes it atomically.
pub fn save_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(v)?)
}

/// Loads JSON from `path`; a missing or unreadable/corrupt file yields `T::default()`.
pub fn load_json_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The directory `crate::paths::data_file` resolves into ("." when none was set).
pub fn data_dir() -> PathBuf {
    let probe = crate::paths::data_file("x");
    match probe.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Unix seconds now.
pub fn now_secs() -> u64 {
    crate::auth::now()
}

/// Days since 1970-01-01 -> (year, month, day). Howard Hinnant's civil_from_days.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// (year, month, day) -> days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix seconds -> "YYYYMMDD-HHMMSS" (UTC), sortable.
pub fn stamp(secs: u64) -> String {
    let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
    let s = secs % 86_400;
    format!("{y:04}{mo:02}{d:02}-{:02}{:02}{:02}", s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ytnative-test-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(stamp(1_700_000_000), "20231114-221320");
    }

    #[test]
    fn atomic_roundtrip() {
        let d = temp_dir("fsutil");
        let p = d.join("sub/x.json");
        save_json(&p, &vec![1, 2, 3]).unwrap();
        let v: Vec<i32> = load_json_or_default(&p);
        assert_eq!(v, [1, 2, 3]);
        assert!(!d.join("sub/x.json.tmp").exists());
        let missing: Vec<i32> = load_json_or_default(&d.join("nope.json"));
        assert!(missing.is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }
}
