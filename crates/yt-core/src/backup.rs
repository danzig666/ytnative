//! Backup / restore of the app's own files as one JSON bundle.
//!
//! A bundle carries `settings.json` (which also holds the blocked channels and other content
//! filters), `channel_groups.json`, `search_history.json` and `player_memory.json`. The signed-in
//! accounts (`accounts.json`) are included only when the user opts in
//! ([`BackupOptions::include_accounts`]); such a bundle is flagged `contains_sign_in_tokens`
//! because anyone holding the file can use those YouTube sign-ins. Restoring accounts is likewise
//! opt-in.
//!
//! Bundles are written to a chosen path (the TV app offers USB storage, see [`usb_dirs`], or the
//! data dir), can be rotated automatically ([`auto_backup`]), and can go to Google Drive through
//! [`crate::gdrive`] via [`to_bytes`] / [`from_bytes`].

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FORMAT: &str = "ytnative-backup";
/// Bumped when the bundle layout changes incompatibly; older versions stay restorable.
pub const BACKUP_VERSION: u32 = 1;
/// Files a bundle may contain besides [`ACCOUNTS_FILE`].
pub const FILES: &[&str] = &[
    "settings.json",
    crate::channel_groups::FILE_NAME,
    crate::search_history::FILE_NAME,
    crate::memory::FILE_NAME,
];
/// Holds the sign-in tokens; only with explicit opt-in.
pub const ACCOUNTS_FILE: &str = "accounts.json";
pub const NAME_PREFIX: &str = "ytnative-backup-";
/// Keep this many automatic backups by default.
pub const DEFAULT_KEEP: usize = 5;

#[derive(Debug, Clone, Copy, Default)]
pub struct BackupOptions {
    /// Also export `accounts.json` (sign-in tokens!).
    pub include_accounts: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RestoreOptions {
    /// Also restore `accounts.json` when the bundle has it (replaces the current sign-ins).
    pub include_accounts: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bundle {
    pub format: String,
    pub version: u32,
    /// Unix seconds.
    pub created_at: u64,
    pub app_version: String,
    /// True when `files` has `accounts.json`: treat the file like a password.
    pub contains_sign_in_tokens: bool,
    /// File name -> its JSON content.
    pub files: BTreeMap<String, Value>,
}

impl Bundle {
    pub fn file_names(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }
}

/// Collects the app files from `data_dir`. Missing files are skipped; a file that is not valid
/// JSON is an error (better to notice than to silently drop it).
pub fn create_bundle_from(data_dir: &Path, opts: BackupOptions) -> Result<Bundle> {
    let mut files = BTreeMap::new();
    let names = FILES.iter().copied().chain(opts.include_accounts.then_some(ACCOUNTS_FILE));
    for name in names {
        let p = data_dir.join(name);
        let Ok(raw) = std::fs::read(&p) else { continue };
        let v: Value = serde_json::from_slice(&raw).with_context(|| format!("{name} is not valid JSON"))?;
        files.insert(name.to_string(), v);
    }
    Ok(Bundle {
        format: FORMAT.into(),
        version: BACKUP_VERSION,
        created_at: crate::fsutil::now_secs(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        contains_sign_in_tokens: files.contains_key(ACCOUNTS_FILE),
        files,
    })
}

/// [`create_bundle_from`] the data dir.
pub fn create_bundle(opts: BackupOptions) -> Result<Bundle> {
    create_bundle_from(&crate::fsutil::data_dir(), opts)
}

pub fn to_bytes(b: &Bundle) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(b)?)
}

/// Parses and validates a bundle: format tag, supported version, only known file names, and
/// the files this crate owns must deserialize into their types.
pub fn from_bytes(bytes: &[u8]) -> Result<Bundle> {
    let b: Bundle = serde_json::from_slice(bytes).context("not a ytnative backup (bad JSON)")?;
    validate(&b)?;
    Ok(b)
}

pub fn validate(b: &Bundle) -> Result<()> {
    if b.format != FORMAT {
        bail!("not a ytnative backup (format \"{}\")", b.format);
    }
    if b.version == 0 || b.version > BACKUP_VERSION {
        bail!("backup version {} is not supported (this app reads up to {BACKUP_VERSION}); update the app", b.version);
    }
    for (name, v) in &b.files {
        if !FILES.contains(&name.as_str()) && name != ACCOUNTS_FILE {
            bail!("unexpected file \"{name}\" in backup");
        }
        if !v.is_object() {
            bail!("{name} in backup is not a JSON object");
        }
        let ok = match name.as_str() {
            crate::channel_groups::FILE_NAME => {
                serde_json::from_value::<crate::channel_groups::Store>(v.clone()).map(drop)
            }
            crate::search_history::FILE_NAME => {
                serde_json::from_value::<crate::search_history::SearchHistory>(v.clone()).map(drop)
            }
            crate::memory::FILE_NAME => serde_json::from_value::<crate::memory::PlayerMemory>(v.clone()).map(drop),
            ACCOUNTS_FILE => serde_json::from_value::<crate::accounts::Store>(v.clone()).map(drop),
            _ => Ok(()),
        };
        ok.with_context(|| format!("{name} in backup is damaged"))?;
    }
    if b.files.contains_key(ACCOUNTS_FILE) && !b.contains_sign_in_tokens {
        bail!("backup has accounts.json but is not flagged as containing sign-in tokens");
    }
    Ok(())
}

pub fn read_bundle(path: &Path) -> Result<Bundle> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    from_bytes(&bytes)
}

/// Writes a bundle of the current data to `path` (atomically). Returns the bundle written.
pub fn export_to(path: &Path, opts: BackupOptions) -> Result<Bundle> {
    let b = create_bundle(opts)?;
    crate::fsutil::write_atomic(path, &to_bytes(&b)?)?;
    Ok(b)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    /// In the bundle but not restored (accounts without opt-in).
    pub skipped: Vec<String>,
}

/// Writes the bundle's files into `data_dir`, each atomically. The bundle is validated first, so
/// nothing is written from a bad bundle. The app should reload its state (or restart) afterwards.
pub fn restore_into(data_dir: &Path, b: &Bundle, opts: RestoreOptions) -> Result<RestoreReport> {
    validate(b)?;
    let mut r = RestoreReport::default();
    for (name, v) in &b.files {
        if name == ACCOUNTS_FILE && !opts.include_accounts {
            r.skipped.push(name.clone());
            continue;
        }
        // accounts.json is written under the accounts lock, so a token refresh saving at the same
        // moment cannot write the old accounts back over the restored ones.
        let _accounts = (name == ACCOUNTS_FILE).then(crate::accounts::lock);
        crate::fsutil::write_atomic(&data_dir.join(name), &serde_json::to_vec_pretty(v)?)?;
        r.restored.push(name.clone());
    }
    Ok(r)
}

/// [`restore_into`] the data dir.
pub fn restore(b: &Bundle, opts: RestoreOptions) -> Result<RestoreReport> {
    restore_into(&crate::fsutil::data_dir(), b, opts)
}

/// Reads, validates and restores a bundle file.
pub fn restore_from(path: &Path, opts: RestoreOptions) -> Result<RestoreReport> {
    restore(&read_bundle(path)?, opts)
}

/// "ytnative-backup-YYYYMMDD-HHMMSS.json" for `secs` (UTC).
pub fn file_name(secs: u64) -> String {
    format!("{NAME_PREFIX}{}.json", crate::fsutil::stamp(secs))
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

/// Bundles named `ytnative-backup-*.json` in `dir`, newest first (by the timestamp in the name).
pub fn list_backups(dir: &Path) -> Vec<BackupInfo> {
    let mut v: Vec<BackupInfo> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.starts_with(NAME_PREFIX) && name.ends_with(".json")).then(|| BackupInfo {
                size: e.metadata().map(|m| m.len()).unwrap_or(0),
                path: e.path(),
                name,
            })
        })
        .collect();
    v.sort_by_key(|b| std::cmp::Reverse(sort_key(&b.name)));
    v
}

/// (timestamp, same-second counter) of a backup name, so "...-120000-10" sorts after
/// "...-120000-2", which sorts after "...-120000".
fn sort_key(name: &str) -> (String, u64) {
    let stem = name.strip_prefix(NAME_PREFIX).unwrap_or(name).trim_end_matches(".json");
    match stem.rsplit_once('-') {
        // The stamp itself has one dash ("YYYYMMDD-HHMMSS"); a second one starts the counter.
        Some((stamp, n)) if stamp.contains('-') => (stamp.to_string(), n.parse().unwrap_or(0)),
        _ => (stem.to_string(), 0),
    }
}

/// Writes a new timestamped bundle of `data_dir` into `dir` and deletes the oldest ones so at most
/// `keep` remain. Returns the new file's path.
pub fn auto_backup_from(data_dir: &Path, dir: &Path, keep: usize, opts: BackupOptions) -> Result<PathBuf> {
    let b = create_bundle_from(data_dir, opts)?;
    let mut path = dir.join(file_name(b.created_at));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{NAME_PREFIX}{}-{n}.json", crate::fsutil::stamp(b.created_at)));
        n += 1;
    }
    crate::fsutil::write_atomic(&path, &to_bytes(&b)?)?;
    // The new file is never deleted, even when it sorts as the oldest (a TV clock that is wrong
    // before the network time arrives): it is the only copy of the current data.
    let others = list_backups(dir).into_iter().filter(|o| o.path != path);
    for old in others.skip(keep.max(1) - 1) {
        let _ = std::fs::remove_file(old.path);
    }
    Ok(path)
}

/// [`auto_backup_from`] the data dir.
pub fn auto_backup(dir: &Path, keep: usize, opts: BackupOptions) -> Result<PathBuf> {
    auto_backup_from(&crate::fsutil::data_dir(), dir, keep, opts)
}

/// Mounted USB storage on webOS: `/tmp/usb/<disk>/<partition>` (e.g. `/tmp/usb/sda/sda1`).
pub fn usb_dirs() -> Vec<PathBuf> {
    usb_dirs_in(Path::new("/tmp/usb"))
}

pub fn usb_dirs_in(root: &Path) -> Vec<PathBuf> {
    let subdirs = |p: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        v.sort();
        v
    };
    let mut out = Vec::new();
    for disk in subdirs(root) {
        let parts = subdirs(&disk);
        if parts.is_empty() {
            out.push(disk);
        } else {
            out.extend(parts);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seed(dir: &Path) {
        std::fs::write(dir.join("settings.json"), r#"{"blocked_channels":["Spam"],"speed":1.25}"#).unwrap();
        let mut g = crate::channel_groups::Store::load_from(&dir.join(crate::channel_groups::FILE_NAME));
        let id = g.create("News");
        g.add_channel(&id, crate::channel_groups::ChannelRef::new("UC1"));
        g.save().unwrap();
        let mut h = crate::search_history::SearchHistory::load_from(&dir.join(crate::search_history::FILE_NAME));
        h.add("rust");
        h.save().unwrap();
        std::fs::write(dir.join(ACCOUNTS_FILE), r#"{"current":"a","accounts":[]}"#).unwrap();
        std::fs::write(dir.join("unrelated.json"), "{}").unwrap();
    }

    #[test]
    fn export_restore_roundtrip() {
        let src = crate::fsutil::temp_dir("backup-src");
        let dst = crate::fsutil::temp_dir("backup-dst");
        seed(&src);

        let b = create_bundle_from(&src, BackupOptions::default()).unwrap();
        assert!(!b.contains_sign_in_tokens);
        assert_eq!(b.file_names(), ["channel_groups.json", "search_history.json", "settings.json"]);

        let b2 = from_bytes(&to_bytes(&b).unwrap()).unwrap();
        assert_eq!(b, b2);
        let r = restore_into(&dst, &b2, RestoreOptions::default()).unwrap();
        assert_eq!(r.restored.len(), 3);
        let s: Value = serde_json::from_slice(&std::fs::read(dst.join("settings.json")).unwrap()).unwrap();
        assert_eq!(s["blocked_channels"][0], "Spam");
        let g = crate::channel_groups::Store::load_from(&dst.join(crate::channel_groups::FILE_NAME));
        assert_eq!(g.groups[0].channels[0].id, "UC1");

        // Accounts only with opt-in, both ways.
        let ba = create_bundle_from(&src, BackupOptions { include_accounts: true }).unwrap();
        assert!(ba.contains_sign_in_tokens);
        let r = restore_into(&dst, &ba, RestoreOptions::default()).unwrap();
        assert_eq!(r.skipped, [ACCOUNTS_FILE]);
        assert!(!dst.join(ACCOUNTS_FILE).exists());
        let r = restore_into(&dst, &ba, RestoreOptions { include_accounts: true }).unwrap();
        assert!(r.restored.contains(&ACCOUNTS_FILE.to_string()));
        assert!(dst.join(ACCOUNTS_FILE).exists());

        std::fs::remove_dir_all(src).unwrap();
        std::fs::remove_dir_all(dst).unwrap();
    }

    #[test]
    fn validation() {
        let base = || Bundle {
            format: FORMAT.into(),
            version: 1,
            created_at: 0,
            app_version: "x".into(),
            contains_sign_in_tokens: false,
            files: BTreeMap::new(),
        };
        assert!(validate(&base()).is_ok());
        let mut b = base();
        b.format = "other".into();
        assert!(validate(&b).is_err());
        let mut b = base();
        b.version = BACKUP_VERSION + 1;
        assert!(validate(&b).unwrap_err().to_string().contains("not supported"));
        let mut b = base();
        b.files.insert("../evil.json".into(), json!({}));
        assert!(validate(&b).is_err());
        let mut b = base();
        b.files.insert("settings.json".into(), json!([1]));
        assert!(validate(&b).is_err());
        let mut b = base();
        b.files.insert(crate::channel_groups::FILE_NAME.into(), json!({"groups": "nope"}));
        assert!(validate(&b).unwrap_err().to_string().contains("damaged"));
        let mut b = base();
        b.files.insert(ACCOUNTS_FILE.into(), json!({}));
        assert!(validate(&b).is_err()); // unflagged tokens
        assert!(from_bytes(b"garbage").is_err());
        // Nothing is written from an invalid bundle.
        let d = crate::fsutil::temp_dir("backup-bad");
        let mut b = base();
        b.files.insert("settings.json".into(), json!({}));
        b.files.insert("x.json".into(), json!({}));
        assert!(restore_into(&d, &b, RestoreOptions::default()).is_err());
        assert!(!d.join("settings.json").exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn rotation() {
        let src = crate::fsutil::temp_dir("backup-rot-src");
        let out = crate::fsutil::temp_dir("backup-rot-out");
        seed(&src);
        // Pre-existing older backups (by name).
        for s in ["20200101-000000", "20210101-000000", "20220101-000000"] {
            std::fs::write(out.join(format!("{NAME_PREFIX}{s}.json")), "{}").unwrap();
        }
        std::fs::write(out.join("keep-me.json"), "{}").unwrap();
        let p1 = auto_backup_from(&src, &out, 2, BackupOptions::default()).unwrap();
        let p2 = auto_backup_from(&src, &out, 2, BackupOptions::default()).unwrap();
        assert_ne!(p1, p2);
        let names: Vec<String> = list_backups(&out).into_iter().map(|b| b.name).collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|n| !n.contains("2020") && !n.contains("2021") && !n.contains("2022")));
        assert!(out.join("keep-me.json").exists());
        assert!(read_bundle(&p2).is_ok());
        std::fs::remove_dir_all(src).unwrap();
        std::fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn rotation_keeps_new_file_with_wrong_clock() {
        let src = crate::fsutil::temp_dir("backup-clock-src");
        let out = crate::fsutil::temp_dir("backup-clock-out");
        seed(&src);
        // Backups "from the future" relative to the current clock.
        for s in ["29990101-000000", "29990102-000000"] {
            std::fs::write(out.join(format!("{NAME_PREFIX}{s}.json")), "{}").unwrap();
        }
        let p = auto_backup_from(&src, &out, 2, BackupOptions::default()).unwrap();
        assert!(p.exists());
        let names: Vec<String> = list_backups(&out).into_iter().map(|b| b.name).collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().any(|n| n.contains("29990102")));
        std::fs::remove_dir_all(src).unwrap();
        std::fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn same_second_counter_sorts_numerically() {
        let mut names = vec![
            format!("{NAME_PREFIX}20240101-120000.json"),
            format!("{NAME_PREFIX}20240101-120000-10.json"),
            format!("{NAME_PREFIX}20240101-120000-2.json"),
            format!("{NAME_PREFIX}20231231-235959-99.json"),
        ];
        names.sort_by_key(|n| std::cmp::Reverse(sort_key(n)));
        assert_eq!(
            names,
            [
                format!("{NAME_PREFIX}20240101-120000-10.json"),
                format!("{NAME_PREFIX}20240101-120000-2.json"),
                format!("{NAME_PREFIX}20240101-120000.json"),
                format!("{NAME_PREFIX}20231231-235959-99.json"),
            ]
        );
    }

    #[test]
    fn usb_layout() {
        let root = crate::fsutil::temp_dir("usb");
        std::fs::create_dir_all(root.join("sda/sda1")).unwrap();
        std::fs::create_dir_all(root.join("sdb")).unwrap();
        let v = usb_dirs_in(&root);
        assert_eq!(v, [root.join("sda/sda1"), root.join("sdb")]);
        assert!(usb_dirs_in(&root.join("missing")).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names() {
        assert_eq!(file_name(1_700_000_000), "ytnative-backup-20231114-221320.json");
    }
}
