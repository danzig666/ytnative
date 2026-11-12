//! `cargo xtask <cmd>`: everything that runs on the developer PC.
//!
//! TV address: env `YTNATIVE_TV`, else the first line of `.tv-host` in the repo root.
//! SSH key: env `YTNATIVE_TV_KEY`, else `~/.ssh/id_lgtv_ytnative`.
//!
//! Commands that touch the TV only write to `/tmp/ytnative` and to our own installed app directory
//! (`/media/developer/apps/usr/palm/applications/<id>`), and only use the TV's normal app
//! installer / application manager. Nothing here touches system partitions.

mod checks;
mod crash;
mod elf;
mod release;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "armv7-unknown-linux-gnueabi";
const TV_TMP: &str = "/tmp/ytnative";
const APPS_DIR: &str = "/media/developer/apps/usr/palm/applications";

const USAGE: &str = "\
cargo xtask <command>

  build                 cross-build the TV app (release)
  ipk                   build + write target/ipk/<id>_<ver>_arm.ipk
  install               ipk + install it on the TV with the TV's app installer
  deploy                build + copy just the binary into the installed app (fast loop)
  run [params-json]     close + launch the app (params passed as launch parameters)
  close                 close the app
  logs [n]              show the app's log on the TV
  uninstall             remove the app from the TV
  ssh <cmd...>          run a command on the TV
  key <tokens...>       press remote keys in the running app (up down left right ok back exit)
  shot [file] [method]  screenshot the TV (DISPLAY = video+UI, VIDEO, GRAPHIC) to target/shots/
  pull-sysroot          copy the TV's shared libraries we link/check against into sysroot/
  symcheck              verify every import of the TV binary resolves against sysroot/lib
                        (transitively: GL symbols come from libmali via libGLESv2.so.2)
  needed [--check|file] print the binary's DT_NEEDED; --check compares with ci/expected-dt-needed.txt
  fwcompat <dir> [-v]   grade the binary's imports (with symbol versions) against every TV
                        snapshot under <dir>/<name>/lib (see docs/debugging.md)
  stub-symbols          regenerate ci/symbols/*.txt from sysroot/lib (link stub lists)
  stubs                 generate the link-only stub libraries into target/stubs/
  crash [log-file]      fetch /tmp/ytnative/crash.log from the TV (or read a local file) and
                        symbolize it against target/.../ytnative.debug
  release-manifest [ipk-url] [out]
                        write the Homebrew Channel manifest for target/ipk/*.ipk

Windows desktop simulator (never touches the TV; needs tools/fetch-sim-deps.ps1 once):
  sim [--release] [--detach] [-- app-args]
                        build + run the app in a 1920x1080 window (data: %LOCALAPPDATA%\\ytnative;
                        the repo's token.json is copied there if no account exists yet)
  sim-key <tokens...>   press keys in the running simulator (up down left right ok back exit)
  sim-shot [name]       save the simulator's next frame as target/shots/<name>.png
";

fn main() {
    if let Err(e) = run() {
        eprintln!("xtask: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root()?;
    std::env::set_current_dir(&root)?;
    match args.first().map(String::as_str) {
        Some("build") => build(),
        Some("ipk") => ipk().map(|_| ()),
        Some("install") => install(),
        Some("deploy") => deploy(),
        Some("run") => launch(args.get(1).map(String::as_str).unwrap_or("{}")),
        Some("close") => close(),
        Some("logs") => logs(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(80)),
        Some("uninstall") => uninstall(),
        Some("ssh") => tv_ssh_print(&args[1..].join(" ")),
        // Only while the app runs: with no reader the FIFO write would block forever.
        Some("key") => tv_ssh_print(&format!(
            "if pidof ytnative >/dev/null; then timeout 5 sh -c 'echo {} > {TV_TMP}/remote' || echo 'key: write timed out'; else echo 'key: app not running'; fi",
            args[1..].join(" ")
        )),
        Some("shot") => shot(args.get(1).map(String::as_str), args.get(2).map(String::as_str).unwrap_or("DISPLAY")),
        Some("pull-sysroot") => pull_sysroot(),
        Some("symcheck") => checks::symcheck(&binary()),
        Some("needed") => match args.get(1).map(String::as_str) {
            Some("--check") => checks::needed(&binary(), true),
            Some(f) => checks::describe_file(Path::new(f)),
            None => checks::needed(&binary(), false),
        },
        Some("fwcompat") => {
            let dir = args.get(1).context("usage: cargo xtask fwcompat <dir-of-sysroots> [-v]")?;
            checks::fwcompat(&binary(), Path::new(dir), args.iter().any(|a| a == "-v" || a == "--verbose"))
        }
        Some("stub-symbols") => checks::stub_symbols(),
        Some("stubs") => stubs(),
        Some("crash") => crash::run(args.get(1).map(String::as_str)),
        Some("release-manifest") => release::manifest(args.get(1).map(String::as_str), args.get(2).map(String::as_str)),
        Some("sim") => sim::run(&args[1..]),
        Some("sim-key") => sim::key(&args[1..]),
        Some("sim-shot") => sim::shot(args.get(1).map(String::as_str)),
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- paths & app info

fn repo_root() -> Result<PathBuf> {
    let mut d = std::env::current_dir()?;
    loop {
        if d.join("xtask").is_dir() && d.join("Cargo.toml").is_file() {
            return Ok(d);
        }
        if !d.pop() {
            bail!("run inside the ytnative repository");
        }
    }
}

fn pkg_dir() -> PathBuf {
    PathBuf::from("crates/app/pkg")
}

fn appinfo() -> Result<Value> {
    let raw = std::fs::read_to_string(pkg_dir().join("appinfo.json")).context("reading appinfo.json")?;
    Ok(serde_json::from_str(&raw)?)
}

fn app_field(key: &str) -> Result<String> {
    appinfo()?[key].as_str().map(str::to_string).with_context(|| format!("appinfo.json has no {key}"))
}

/// What cargo links (line tables, symbols): never shipped.
fn linked_binary() -> PathBuf {
    PathBuf::from(format!("target/{TARGET}/release/ytnative"))
}

/// The stripped binary that goes into the IPK / onto the TV.
fn binary() -> PathBuf {
    PathBuf::from(format!("target/{TARGET}/release/dist/ytnative"))
}

/// The unstripped twin of `binary()`, kept for symbolizing crash logs.
fn debug_binary() -> PathBuf {
    PathBuf::from(format!("target/{TARGET}/release/dist/ytnative.debug"))
}

// ---------------------------------------------------------------- TV access

fn tv_host() -> Result<String> {
    if let Ok(h) = std::env::var("YTNATIVE_TV") {
        return Ok(h);
    }
    let h = std::fs::read_to_string(".tv-host").context("set YTNATIVE_TV or write the TV address to .tv-host")?;
    Ok(h.lines().next().unwrap_or("").trim().to_string())
}

fn tv_key() -> PathBuf {
    std::env::var("YTNATIVE_TV_KEY").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default())
            .join(".ssh/id_lgtv_ytnative")
    })
}

fn ssh_base() -> Result<Vec<String>> {
    Ok(vec![
        "-i".into(),
        tv_key().to_string_lossy().into_owned(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=8".into(),
    ])
}

/// Runs `cmd` on the TV with a TTY (luna-send only prints with one) and returns its output.
fn tv_ssh(cmd: &str) -> Result<String> {
    let out = Command::new("ssh")
        .args(ssh_base()?)
        .arg("-tt")
        .arg(format!("root@{}", tv_host()?))
        .arg(format!("{cmd}; exit"))
        .output()
        .context("running ssh")?;
    Ok(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"))
}

fn tv_ssh_print(cmd: &str) -> Result<()> {
    print!("{}", tv_ssh(cmd)?);
    Ok(())
}

fn scp_to_tv(local: &Path, remote: &str) -> Result<()> {
    let st = Command::new("scp")
        .args(ssh_base()?)
        .arg("-q")
        .arg(local)
        .arg(format!("root@{}:{remote}", tv_host()?))
        .status()?;
    if !st.success() {
        bail!("scp {} failed", local.display());
    }
    Ok(())
}

fn luna(uri: &str, payload: &str) -> Result<String> {
    // Single-quote the payload for the TV's shell.
    let p = payload.replace('\'', r"'\''");
    tv_ssh(&format!("luna-send -w 15000 -n 1 {uri} '{p}'"))
}

// ---------------------------------------------------------------- build & package

fn build() -> Result<()> {
    let st = Command::new("cargo")
        .args(["build", "--release", "-p", "ytnative-app", "--target", TARGET])
        .status()?;
    if !st.success() {
        bail!("cross build failed");
    }
    // Keep the linked binary (line tables + symbols) as ytnative.debug and ship a stripped copy.
    std::fs::create_dir_all(binary().parent().unwrap())?;
    std::fs::copy(linked_binary(), debug_binary())?;
    let st = Command::new("python")
        .args(["-m", "ziglang", "objcopy", "--strip-all"])
        .arg(linked_binary())
        .arg(binary())
        .status()
        .context("running zig objcopy")?;
    if !st.success() {
        bail!("zig objcopy --strip-all failed");
    }
    let size = std::fs::metadata(binary())?.len();
    let dbg = std::fs::metadata(debug_binary())?.len();
    println!("built {} ({} KiB; {} KiB with debug info in ytnative.debug)", binary().display(), size / 1024, dbg / 1024);
    checks::needed(&binary(), true)
}

/// Writes an IPK the way plx-native's ci/mkipk.py does: deterministic tar members with explicit
/// modes (Windows has no +x), packageinfo.json, a real Installed-Size, and an `ar` container with
/// bare member names and debian-binary first (GNU-style `name/` makes appinstalld fail with -5).
fn ipk() -> Result<PathBuf> {
    build()?;
    let info = appinfo()?;
    let id = app_field("id")?;
    let version = app_field("version")?;
    let main_exe = app_field("main")?;

    // Collect the app directory: pkg/* plus the freshly built binary.
    let mut files: Vec<(String, Vec<u8>, u32)> = Vec::new();
    for entry in std::fs::read_dir(pkg_dir())? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file() && name != main_exe {
            files.push((name, std::fs::read(entry.path())?, 0o644));
        }
    }
    files.push((main_exe.clone(), std::fs::read(binary())?, 0o755));
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let packageinfo = serde_json::to_vec_pretty(&serde_json::json!({
        "app": id,
        "id": id,
        "loc_name": info["title"],
        "package_format_version": 2,
        "vendor": info["vendor"],
        "version": version,
    }))?;

    let app_prefix = format!("usr/palm/applications/{id}");
    let pkg_prefix = format!("usr/palm/packages/{id}");
    let mut entries: Vec<(String, Option<(Vec<u8>, u32)>)> = Vec::new();
    for d in ["usr", "usr/palm", "usr/palm/applications", "usr/palm/packages", &app_prefix, &pkg_prefix] {
        entries.push((d.to_string(), None));
    }
    for (name, data, mode) in &files {
        entries.push((format!("{app_prefix}/{name}"), Some((data.clone(), *mode))));
    }
    entries.push((format!("{pkg_prefix}/packageinfo.json"), Some((packageinfo, 0o644))));
    let installed_kib: u64 = entries.iter().filter_map(|e| e.1.as_ref()).map(|(d, _)| d.len() as u64).sum::<u64>().div_ceil(1024);
    let data_tgz = targz(&entries)?;

    let control = format!(
        "Package: {id}\nVersion: {version}\nSection: misc\nPriority: optional\nArchitecture: arm\n\
         Installed-Size: {installed_kib}\nMaintainer: {}\nHomepage: https://github.com/danzig666/ytnative\n\
         License: GPL-3.0-or-later\nDescription: {}\nwebOS-Package-Format-Version: 2\nwebOS-Packager-Version: ytnative-xtask/1\n",
        info["vendor"].as_str().unwrap_or("ytnative"),
        info["title"].as_str().unwrap_or("ytnative"),
    );
    let control_tgz = targz(&[("control".into(), Some((control.into_bytes(), 0o644)))])?;

    let mut ar = b"!<arch>\n".to_vec();
    for (name, blob) in [("debian-binary", b"2.0\n".to_vec()), ("control.tar.gz", control_tgz), ("data.tar.gz", data_tgz)] {
        ar.extend(format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", 0, 0, 0, "100644", blob.len()).as_bytes());
        ar.extend(&blob);
        if blob.len() % 2 == 1 {
            ar.push(b'\n');
        }
    }
    std::fs::create_dir_all("target/ipk")?;
    let out = PathBuf::from(format!("target/ipk/{id}_{version}_arm.ipk"));
    std::fs::write(&out, ar)?;
    println!("wrote {} ({} KiB, Installed-Size {installed_kib} KiB)", out.display(), std::fs::metadata(&out)?.len() / 1024);
    Ok(out)
}

fn targz(entries: &[(String, Option<(Vec<u8>, u32)>)]) -> Result<Vec<u8>> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let mut tb = tar::Builder::new(gz);
    tb.mode(tar::HeaderMode::Deterministic);
    for (path, content) in entries {
        let mut h = tar::Header::new_gnu();
        h.set_uid(0);
        h.set_gid(0);
        h.set_username("root")?;
        h.set_groupname("root")?;
        h.set_mtime(0);
        match content {
            None => {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_mode(0o755);
                h.set_size(0);
                h.set_cksum();
                tb.append_data(&mut h, format!("{path}/"), std::io::empty())?;
            }
            Some((data, mode)) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(*mode);
                h.set_size(data.len() as u64);
                h.set_cksum();
                tb.append_data(&mut h, path, data.as_slice())?;
            }
        }
    }
    let gz = tb.into_inner()?;
    let mut out = gz.finish()?;
    out.flush()?;
    Ok(out)
}

// ---------------------------------------------------------------- TV operations

fn install() -> Result<()> {
    let ipk = ipk()?;
    let id = app_field("id")?;
    tv_ssh(&format!("mkdir -p {TV_TMP} && chmod 777 {TV_TMP}"))?;
    scp_to_tv(&ipk, &format!("{TV_TMP}/app.ipk"))?;
    let out = tv_ssh(&format!(
        "luna-send -w 60000 -i luna://com.webos.appInstallService/dev/install \
         '{{\"id\":\"{id}\",\"ipkUrl\":\"{TV_TMP}/app.ipk\",\"subscribe\":true}}' | grep -m1 -o '\"state\":\"installed\"\\|errorText[^,]*'"
    ))?;
    if !out.contains("installed") {
        bail!("install failed: {}", out.trim());
    }
    println!("installed {id}");
    Ok(())
}

fn deploy() -> Result<()> {
    build()?;
    let id = app_field("id")?;
    let exe = app_field("main")?;
    let dir = format!("{APPS_DIR}/{id}");
    let check = tv_ssh(&format!("test -d {dir} && echo yes"))?;
    if !check.contains("yes") {
        bail!("{id} is not installed yet: run `cargo xtask install` once");
    }
    close().ok();
    scp_to_tv(&binary(), &format!("{dir}/{exe}.new"))?;
    tv_ssh(&format!("mv {dir}/{exe}.new {dir}/{exe} && chmod 755 {dir}/{exe}"))?;
    println!("deployed binary to {dir}/{exe}");
    Ok(())
}

fn close() -> Result<()> {
    let id = app_field("id")?;
    let r = luna("luna://com.webos.applicationManager/closeByAppId", &format!(r#"{{"id":"{id}"}}"#))?;
    println!("close: {}", r.trim());
    Ok(())
}

fn launch(params: &str) -> Result<()> {
    let id = app_field("id")?;
    serde_json::from_str::<Value>(params).context("launch params must be JSON")?;
    // SAM keeps a stale "running" state after a hard kill; plx-native always closes first.
    let _ = luna("luna://com.webos.applicationManager/closeByAppId", &format!(r#"{{"id":"{id}"}}"#));
    let r = luna("luna://com.webos.applicationManager/launch", &format!(r#"{{"id":"{id}","params":{params}}}"#))?;
    println!("launch: {}", r.trim());
    Ok(())
}

fn logs(n: usize) -> Result<()> {
    tv_ssh_print(&format!("tail -n {n} {TV_TMP}/ytnative.log 2>/dev/null || echo '(no log yet)'"))
}

fn uninstall() -> Result<()> {
    let id = app_field("id")?;
    let r = luna("luna://com.webos.appInstallService/dev/remove", &format!(r#"{{"id":"{id}"}}"#))?;
    println!("remove: {}", r.trim());
    Ok(())
}

/// Screen capture through the TV's own capture service, which taps the final panel output
/// (method DISPLAY composites the video plane and the UI plane, as plx-native found).
fn shot(file: Option<&str>, method: &str) -> Result<()> {
    std::fs::create_dir_all("target/shots")?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let local = file.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("target/shots/tv-{stamp}.jpg")));
    let fmt = if local.extension().is_some_and(|e| e == "png") { "PNG" } else { "JPEG" };
    let remote = format!("{TV_TMP}/shot.{}", fmt.to_lowercase().replace("jpeg", "jpg"));
    let payload = format!(r#"{{"path":"{remote}","method":"{method}","width":1920,"height":1080,"format":"{fmt}"}}"#);
    let r = tv_ssh(&format!("mkdir -p {TV_TMP}; luna-send -w 15000 -n 1 luna://com.webos.service.capture/executeOneShot '{payload}'"))?;
    if !r.contains("\"returnValue\": true") && !r.contains("\"returnValue\":true") {
        bail!("capture failed: {}", r.trim());
    }
    let st = Command::new("scp").args(ssh_base()?).arg("-q").arg(format!("root@{}:{remote}", tv_host()?)).arg(&local).status()?;
    if !st.success() {
        bail!("could not copy the capture");
    }
    tv_ssh(&format!("rm -f {remote}"))?;
    println!("{}", local.display());
    Ok(())
}

// ---------------------------------------------------------------- sysroot & symbol check

/// Shared libraries the app links against or dlopens. Pulled once from the TV so the build and the
/// symbol check see exactly what this firmware ships.
const SYSROOT_LIBS: &[&str] = &[
    "/lib/libc.so.6",
    "/lib/libm.so.6",
    "/lib/libdl.so.2",
    "/lib/libpthread.so.0",
    "/lib/librt.so.1",
    "/lib/libgcc_s.so.1",
    "/usr/lib/libSDL2-2.0.so.0",
    "/usr/lib/libSDL2_ttf-2.0.so.0",
    "/usr/lib/libGLESv2.so.2",
    // libGLESv2 on LG sets is a stub; the GL entry points live in the Mali driver.
    "/usr/lib/libmali.so.0",
    "/usr/lib/libEGL.so.1",
    "/usr/lib/libwayland-client.so.0",
    "/usr/lib/libluna-service2.so.3",
    "/usr/lib/libglib-2.0.so.0",
    "/usr/lib/libcurl.so.4",
    "/usr/lib/libplayerAPIs.so.1",
    // Indirect dependencies of the above, so `symcheck`/`fwcompat` can walk the whole closure.
    "/lib/ld-linux.so.3",
    "/usr/lib/libstdc++.so.6",
    "/usr/lib/libfreetype.so.6",
    "/usr/lib/libwayland-server.so.0",
    "/usr/lib/libffi.so.8",
    "/usr/lib/libPmLogLib.so.3",
    "/usr/lib/libpbnjson_c.so.2",
    "/usr/lib/libpcre.so.1",
];

fn pull_sysroot() -> Result<()> {
    std::fs::create_dir_all("sysroot/lib")?;
    let host = tv_host()?;
    for lib in SYSROOT_LIBS {
        // -L follows the symlink so we get the real file under the soname.
        let resolved = tv_ssh(&format!("readlink -f {lib} 2>/dev/null"))?;
        let resolved = resolved.trim();
        if resolved.is_empty() {
            println!("  missing on TV: {lib}");
            continue;
        }
        let name = Path::new(lib).file_name().unwrap().to_string_lossy().into_owned();
        let st = Command::new("scp")
            .args(ssh_base()?)
            .arg("-q")
            .arg(format!("root@{host}:{resolved}"))
            .arg(format!("sysroot/lib/{name}"))
            .status()?;
        println!("  {} {lib}", if st.success() { "ok     " } else { "FAILED " });
    }
    let fw = tv_ssh("grep -E '^VERSION=' /etc/os-release; cat /var/run/nyx/device_info.json 2>/dev/null | grep -o '\"hardware_id\": *\"[^\"]*\"'")?;
    std::fs::write("sysroot/FIRMWARE.txt", fw)?;
    println!("sysroot/ updated (not committed: LG binaries)");
    Ok(())
}

/// Link-only stub libraries (target/stubs/) from ci/symbols/*.txt; crates/app/build.rs runs the
/// same generator, this is for running it by hand.
fn stubs() -> Result<()> {
    let st = Command::new("python").args(["tools/gen-stubs.py", "--out", "target/stubs"]).status().context("running python")?;
    if !st.success() {
        bail!("stub generation failed");
    }
    Ok(())
}

/// The Windows desktop simulator (`crates/app` built for the host with `--features sim`). Nothing
/// here talks to the TV.
mod sim {
    use anyhow::{bail, Context, Result};
    use std::io::{Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn port() -> u16 {
        std::env::var("YTNATIVE_SIM_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(47800)
    }

    fn data_dir() -> Result<PathBuf> {
        let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set (the simulator is Windows-only)")?;
        Ok(PathBuf::from(base).join("ytnative"))
    }

    /// The signed-in token for testing: `YTNATIVE_SIM_TOKEN`, else `token.json` in this checkout,
    /// else in the main checkout (when this is a git worktree).
    fn token_source() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("YTNATIVE_SIM_TOKEN") {
            return Some(PathBuf::from(p));
        }
        let here = PathBuf::from("token.json");
        if here.is_file() {
            return Some(here);
        }
        let out = Command::new("git").args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).output().ok()?;
        let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        let main = common.parent()?.join("token.json");
        main.is_file().then_some(main)
    }

    /// Copies the token into the simulator's data dir unless an account is already there. Never
    /// prints the token itself.
    fn seed_token(data: &Path) -> Result<()> {
        std::fs::create_dir_all(data)?;
        if data.join("accounts.json").exists() || data.join("token.json").exists() {
            return Ok(());
        }
        match token_source() {
            Some(src) => {
                std::fs::copy(&src, data.join("token.json")).with_context(|| format!("copying {}", src.display()))?;
                println!("sim: copied the signed-in token from {} into {}", src.display(), data.display());
            }
            None => println!("sim: no token.json found; the app starts signed out"),
        }
        Ok(())
    }

    pub fn run(args: &[String]) -> Result<()> {
        let release = args.iter().any(|a| a == "--release");
        let detach = args.iter().any(|a| a == "--detach");
        let app_args: Vec<&String> = args.iter().skip_while(|a| *a != "--").skip(1).collect();
        if !Path::new("third_party/win").is_dir() {
            bail!("third_party/win is missing: run `powershell -ExecutionPolicy Bypass -File tools/fetch-sim-deps.ps1` once");
        }
        let mut build = Command::new("cargo");
        build.args(["build", "-p", "ytnative-app", "--features", "sim"]);
        if release {
            build.arg("--release");
        }
        if !build.status()?.success() {
            bail!("simulator build failed");
        }
        let exe = PathBuf::from(format!("target/{}/ytnative.exe", if release { "release" } else { "debug" }));
        let data = data_dir()?;
        seed_token(&data)?;
        let shots = std::env::current_dir()?.join("target").join("shots");
        let mut cmd = Command::new(&exe);
        cmd.args(app_args).env("YTNATIVE_SHOT_DIR", &shots);
        println!("sim: {} (keys: 127.0.0.1:{}, shots: {}, data: {})", exe.display(), port(), shots.display(), data.display());
        if detach {
            let log = std::fs::File::create("target/sim.log")?;
            cmd.stdout(log.try_clone()?).stderr(log);
            let child = cmd.spawn().context("starting the simulator")?;
            println!("sim: running detached as pid {} (log: target/sim.log)", child.id());
            return Ok(());
        }
        let st = cmd.status().context("starting the simulator")?;
        if !st.success() {
            bail!("simulator exited with {st}");
        }
        Ok(())
    }

    fn send(line: &str) -> Result<String> {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port()))
            .with_context(|| format!("no simulator listening on 127.0.0.1:{} (start it with `cargo xtask sim`)", port()))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(120)))?;
        s.write_all(format!("{line}\n").as_bytes())?;
        let mut reply = String::new();
        s.read_to_string(&mut reply).context("waiting for the simulator")?;
        Ok(reply.trim().to_string())
    }

    pub fn key(tokens: &[String]) -> Result<()> {
        if tokens.is_empty() {
            bail!("usage: cargo xtask sim-key <up|down|left|right|ok|back|exit>...");
        }
        let r = send(&tokens.join(" "))?;
        if r != "ok" {
            println!("{r}");
        }
        Ok(())
    }

    pub fn shot(name: Option<&str>) -> Result<()> {
        let tok = match name {
            Some(n) => format!("shot:{}", n.trim_end_matches(".png")),
            None => "shot".into(),
        };
        let r = send(&tok)?;
        match r.lines().find_map(|l| l.strip_prefix("shot ")) {
            Some(path) => {
                println!("{path}");
                Ok(())
            }
            None => bail!("simulator: {r}"),
        }
    }
}
