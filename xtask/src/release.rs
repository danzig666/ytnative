//! `cargo xtask release-manifest [ipk-url] [out]`: the webOS Homebrew Channel package manifest
//! (webosbrew `PackageManifest` schema) for the IPK in target/ipk/.
//!
//! The manifest, the IPK and a one-package `repo.json` index are attached to the same GitHub
//! release; `releases/latest/download/<id>.manifest.json` is then a self-updating entry point and
//! `releases/latest/download/repo.json` a custom-repository URL (docs/release.md).

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const REPO_URL: &str = "https://github.com/danzig666/ytnative";

pub fn manifest(ipk_url: Option<&str>, out: Option<&str>) -> Result<()> {
    let info = crate::appinfo()?;
    let id = crate::app_field("id")?;
    let version = crate::app_field("version")?;
    let file = format!("{id}_{version}_arm.ipk");
    let ipk = PathBuf::from("target/ipk").join(&file);
    let data = std::fs::read(&ipk).with_context(|| format!("reading {} (run `cargo xtask ipk` first)", ipk.display()))?;
    let sha = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect::<String>();
    // Must be https: and answer 200 (webosbrew lints it). Pinned to the release tag so an old
    // manifest keeps pointing at its own icon.
    let icon = std::env::var("YTNATIVE_ICON_URI")
        .unwrap_or_else(|_| format!("https://raw.githubusercontent.com/danzig666/ytnative/v{version}/crates/app/pkg/icon.png"));
    let m = serde_json::json!({
        "id": id,
        "version": version,
        "type": info["type"].as_str().unwrap_or("native"),
        "title": info["title"],
        "appDescription": "SmartTube ported to LG webOS: a native (Rust) YouTube client for the TV, without ads",
        "iconUri": icon,
        "sourceUrl": REPO_URL,
        "rootRequired": false,
        // Absolute and pinned to the tag: the manifest and the IPK it hashes can never disagree,
        // whichever URL the manifest itself was fetched from.
        "ipkUrl": ipk_url.map(str::to_string).unwrap_or_else(|| format!("{REPO_URL}/releases/download/v{version}/{file}")),
        "ipkHash": { "sha256": sha },
        "ipkSize": data.len(),
    });
    let manifest_name = format!("{id}.manifest.json");
    let out = out.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("target/ipk").join(&manifest_name));
    std::fs::write(&out, serde_json::to_string_pretty(&m)? + "\n")?;
    println!("wrote {} (ipk sha256 {sha})", out.display());

    // Custom-repository index (Homebrew Channel > Settings > Add repository takes its URL), in the
    // same shape as repo.webosbrew.org/api/apps.json: paging + packages, each with its manifest
    // embedded (the Channel reads the version from it for "update available"). It is regenerated
    // and republished with every release, so the manifest URL is pinned to this tag.
    let repo = serde_json::json!({
        "paging": { "page": 1, "count": 1, "maxPage": 1, "itemsTotal": 1, "prevUrl": null, "nextUrl": null },
        "packages": [{
            "id": id,
            "title": info["title"],
            "iconUri": icon,
            "manifestUrl": format!("{REPO_URL}/releases/download/v{version}/{manifest_name}"),
            "manifest": m,
            "pool": "main",
            "shortDescription": "SmartTube for LG webOS: a native YouTube client without ads",
        }],
    });
    let repo_out = out.with_file_name("repo.json");
    std::fs::write(&repo_out, serde_json::to_string_pretty(&repo)? + "\n")?;
    println!("wrote {}", repo_out.display());
    Ok(())
}
