//! YouTube's JavaScript challenges, solved with yt-dlp's solver (vendor/ejs, public domain) on
//! QuickJS (vendor/quickjs, compiled in by build.rs).
//!
//! The TV client that takes the signed-in user's token (what private videos need) hands out
//! stream URLs whose `n` parameter must first be transformed by a function in the player script,
//! or the media server answers 403; some formats also carry a `signatureCipher` (the `s` value
//! goes through another function). The solver parses the whole player script once and keeps a
//! much smaller "preprocessed" copy with just those functions; that copy is cached on disk per
//! player build, so the slow parse happens once per YouTube player release.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const SOLVER_LIB: &str = include_str!("../../../vendor/ejs/yt.solver.lib.min.js");
const SOLVER_CORE: &str = include_str!("../../../vendor/ejs/yt.solver.core.min.js");
/// Bumped when the solver scripts change, so cached preprocessed players made by the old ones are
/// not reused.
const SOLVER_VERSION: &str = "ejs-0.8.0";

/// The first parse of a ~2.5 MB player script takes a few seconds on a PC and far longer on a
/// TV's CPU; anything past this is treated as a hung script.
const PARSE_TIMEOUT_SECS: u32 = 300;
const SOLVE_TIMEOUT_SECS: u32 = 60;
const MEM_LIMIT: usize = 512 << 20;
/// QuickJS's own recursion limit, inside the thread's stack below (meriyah recurses deeply).
const JS_STACK: usize = 48 << 20;
const THREAD_STACK: usize = 64 << 20;

extern "C" {
    fn ytjs_eval(
        code: *const u8,
        len: usize,
        timeout_secs: u32,
        mem_limit: usize,
        stack_limit: usize,
        out: *mut *mut u8,
        out_len: *mut usize,
    ) -> i32;
    fn ytjs_free(p: *mut u8);
}

/// Evaluates `code` as a global script on a fresh QuickJS runtime and returns the string value of
/// its last expression. Runs on its own thread with a large stack.
pub fn eval(code: &str, timeout_secs: u32) -> Result<String> {
    let mut src = Vec::with_capacity(code.len() + 1);
    src.extend_from_slice(code.as_bytes());
    src.push(0);
    let worker = std::thread::Builder::new()
        .name("jsc".into())
        .stack_size(THREAD_STACK)
        .spawn(move || {
            let (mut out, mut out_len) = (std::ptr::null_mut(), 0usize);
            let rc = unsafe { ytjs_eval(src.as_ptr(), src.len() - 1, timeout_secs, MEM_LIMIT, JS_STACK, &mut out, &mut out_len) };
            let text = if out.is_null() {
                String::new()
            } else {
                let s = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(out, out_len) }).into_owned();
                unsafe { ytjs_free(out) };
                s
            };
            (rc, text)
        })
        .context("starting the JavaScript thread")?;
    let (rc, text) = worker.join().map_err(|_| anyhow!("the JavaScript thread panicked"))?;
    match rc {
        0 => Ok(text),
        1 => bail!("JavaScript error: {}", text.lines().take(6).collect::<Vec<_>>().join(" | ")),
        _ => bail!("the JavaScript engine could not start"),
    }
}

/// Runs the solver on `input` (its JSON input, see vendor/ejs) and returns its output.
fn run_solver(input: &Value, timeout_secs: u32) -> Result<Value> {
    let code = format!("{SOLVER_LIB}\nObject.assign(globalThis, lib);\n{SOLVER_CORE}\nJSON.stringify(jsc({input}));\n");
    let out: Value = serde_json::from_str(&eval(&code, timeout_secs)?).context("solver output is not JSON")?;
    if out["type"] == "error" {
        bail!("solver: {}", out["error"].as_str().unwrap_or("unknown error"));
    }
    Ok(out)
}

/// The player id ("6b3d0364") in a player script URL (`/s/player/<id>/...`).
pub fn player_id(url: &str) -> Option<&str> {
    let rest = &url[url.find("/s/player/")? + "/s/player/".len()..];
    let id = &rest[..rest.find('/')?];
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())).then_some(id)
}

/// `signatureTimestamp` of a player script (sent in the player request so the URLs match it).
pub fn signature_timestamp(script: &str) -> Option<u32> {
    let i = script.find("signatureTimestamp:")? + "signatureTimestamp:".len();
    let digits: String = script[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// A player build, ready to solve its challenges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    pub url: String,
    pub signature_timestamp: Option<u32>,
    solver: String,
    preprocessed: String,
}

/// Known answers of a sample challenge, to prove a freshly prepared player works before caching.
const PROBE_N: &str = "5cNpZqIJ7ixNqU68Y7S";

static LOADED: Mutex<Option<Arc<Player>>> = Mutex::new(None);

fn cache_file(id: &str) -> std::path::PathBuf {
    crate::paths::data_file(&format!("player-{id}.json"))
}

impl Player {
    /// The player at `url`, from memory, the disk cache, or downloaded and preprocessed (slow,
    /// once per player build). Older cache files are removed when a new build is prepared.
    pub fn load(http: &reqwest::blocking::Client, url: &str) -> Result<Arc<Player>> {
        let id = player_id(url).with_context(|| format!("no player id in {url}"))?.to_string();
        if let Some(p) = LOADED.lock().unwrap().as_ref().filter(|p| p.url == url) {
            return Ok(p.clone());
        }
        let cached = std::fs::read_to_string(cache_file(&id))
            .ok()
            .and_then(|s| serde_json::from_str::<Player>(&s).ok())
            .filter(|p| p.url == url && p.solver == SOLVER_VERSION);
        let p = match cached {
            Some(p) => p,
            None => {
                let p = Self::prepare(http, url)?;
                Self::forget_others(&id);
                if let Ok(s) = serde_json::to_string(&p) {
                    let _ = crate::fsutil::write_atomic(&cache_file(&id), s.as_bytes());
                }
                p
            }
        };
        let p = Arc::new(p);
        *LOADED.lock().unwrap() = Some(p.clone());
        Ok(p)
    }

    fn prepare(http: &reqwest::blocking::Client, url: &str) -> Result<Player> {
        let full = if url.starts_with("http") { url.to_string() } else { format!("https://www.youtube.com{url}") };
        let t0 = std::time::Instant::now();
        let script = http
            .get(&full)
            .header("User-Agent", crate::bootstrap::TV_UA)
            .send()?
            .error_for_status()?
            .text()
            .context("downloading the player script")?;
        let input = json!({
            "type": "player",
            "player": script,
            "requests": [{ "type": "n", "challenges": [PROBE_N] }],
            "output_preprocessed": true,
        });
        let out = run_solver(&input, PARSE_TIMEOUT_SECS)?;
        let n = &out["responses"][0];
        if n["type"] != "result" || n["data"][PROBE_N].as_str().is_none_or(|s| s == PROBE_N) {
            bail!("the player's n function was not found ({})", n["error"].as_str().unwrap_or("no answer"));
        }
        let preprocessed = out["preprocessed_player"].as_str().context("solver returned no preprocessed player")?.to_string();
        eprintln!(
            "[jsc] player {} prepared in {:.1}s ({} KiB -> {} KiB)",
            player_id(url).unwrap_or("?"),
            t0.elapsed().as_secs_f32(),
            script.len() / 1024,
            preprocessed.len() / 1024
        );
        Ok(Player { url: url.to_string(), signature_timestamp: signature_timestamp(&script), solver: SOLVER_VERSION.into(), preprocessed })
    }

    fn forget_others(keep: &str) {
        let Some(dir) = cache_file(keep).parent().map(|d| d.to_path_buf()) else { return };
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("player-") && name.ends_with(".json") && name != format!("player-{keep}.json") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Solves `n` and `sig` challenges: maps from each challenge to its answer.
    pub fn solve(&self, n: &[String], sig: &[String]) -> Result<(HashMap<String, String>, HashMap<String, String>)> {
        let mut requests = Vec::new();
        if !n.is_empty() {
            requests.push(json!({ "type": "n", "challenges": n }));
        }
        if !sig.is_empty() {
            requests.push(json!({ "type": "sig", "challenges": sig }));
        }
        if requests.is_empty() {
            return Ok(Default::default());
        }
        let input = json!({ "type": "preprocessed", "preprocessed_player": self.preprocessed, "requests": requests });
        let out = run_solver(&input, SOLVE_TIMEOUT_SECS)?;
        let mut answers = [HashMap::new(), HashMap::new()];
        let kinds: Vec<usize> = [(!n.is_empty()).then_some(0), (!sig.is_empty()).then_some(1)].into_iter().flatten().collect();
        for (r, kind) in out["responses"].as_array().into_iter().flatten().zip(kinds) {
            if r["type"] != "result" {
                bail!("{} challenge: {}", ["n", "sig"][kind], r["error"].as_str().unwrap_or("no answer"));
            }
            for (k, v) in r["data"].as_object().into_iter().flatten() {
                if let Some(v) = v.as_str() {
                    answers[kind].insert(k.clone(), v.to_string());
                }
            }
        }
        let [n, sig] = answers;
        Ok((n, sig))
    }
}

/// Query parameter `key` of `url` (raw, still percent-encoded).
fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let q = &url[url.find('?')? + 1..];
    q.split('&').find_map(|kv| kv.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

/// `url` with query parameter `key` set to `value` (appended when absent).
fn set_query_param(url: &str, key: &str, value: &str) -> String {
    let Some(qi) = url.find('?') else { return format!("{url}?{key}={value}") };
    let (base, q) = (&url[..qi], &url[qi + 1..]);
    let mut found = false;
    let parts: Vec<String> = q
        .split('&')
        .map(|kv| {
            if kv.split('=').next() == Some(key) {
                found = true;
                format!("{key}={value}")
            } else {
                kv.to_string()
            }
        })
        .collect();
    let mut out = format!("{base}?{}", parts.join("&"));
    if !found {
        out.push_str(&format!("&{key}={value}"));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Fields of an `application/x-www-form-urlencoded` string (a `signatureCipher`).
fn form_fields(s: &str) -> HashMap<String, String> {
    s.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), percent_decode(v))).collect()
}

/// Gives every format in `formats` (a player response's `adaptiveFormats` / `formats`) a playable
/// `url`: deciphers `signatureCipher` and transforms the `n` parameter. Formats that cannot be
/// fixed lose their `url`. Returns how many formats were fixed.
pub fn decipher_formats(player: &Player, formats: &mut [Value]) -> Result<usize> {
    let mut ns: Vec<String> = Vec::new();
    let mut sigs: Vec<String> = Vec::new();
    for f in formats.iter() {
        let url = match f["signatureCipher"].as_str().or(f["cipher"].as_str()) {
            Some(c) => {
                let fields = form_fields(c);
                if let Some(s) = fields.get("s") {
                    sigs.push(s.clone());
                }
                fields.get("url").cloned()
            }
            None => f["url"].as_str().map(str::to_string),
        };
        if let Some(n) = url.as_deref().and_then(|u| query_param(u, "n")) {
            ns.push(percent_decode(n));
        }
    }
    ns.sort();
    ns.dedup();
    sigs.sort();
    sigs.dedup();
    let (n_map, sig_map) = player.solve(&ns, &sigs)?;
    let mut fixed = 0;
    for f in formats.iter_mut() {
        let cipher = f["signatureCipher"].as_str().or(f["cipher"].as_str()).map(form_fields);
        let mut url = match &cipher {
            Some(c) => {
                let (Some(u), Some(s)) = (c.get("url"), c.get("s")) else { continue };
                let Some(sig) = sig_map.get(s) else {
                    f.as_object_mut().map(|o| o.remove("url"));
                    continue;
                };
                let sp = c.get("sp").map(String::as_str).unwrap_or("signature");
                set_query_param(u, sp, &percent_encode(sig))
            }
            None => match f["url"].as_str() {
                Some(u) => u.to_string(),
                None => continue,
            },
        };
        if let Some(n) = query_param(&url, "n").map(percent_decode) {
            match n_map.get(&n) {
                Some(answer) => url = set_query_param(&url, "n", &percent_encode(answer)),
                None => {
                    f.as_object_mut().map(|o| o.remove("url"));
                    continue;
                }
            }
        }
        if let Some(o) = f.as_object_mut() {
            o.remove("signatureCipher");
            o.remove("cipher");
            o.insert("url".into(), Value::String(url));
        }
        fixed += 1;
    }
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quickjs_evaluates_a_script() {
        assert_eq!(eval("let a = [1, 2, 3]; JSON.stringify(a.map(x => x * 2))", 5).unwrap(), "[2,4,6]");
        let e = eval("throw new Error('boom')", 5).unwrap_err().to_string();
        assert!(e.contains("boom"), "{e}");
        assert!(eval("while (true) {}", 1).is_err(), "a runaway script is interrupted");
    }

    #[test]
    fn the_solver_scripts_load() {
        let out = eval(&format!("{SOLVER_LIB}\nObject.assign(globalThis, lib);\n{SOLVER_CORE}\ntypeof jsc"), 30).unwrap();
        assert_eq!(out, "function");
    }

    #[test]
    fn player_ids_and_timestamps() {
        assert_eq!(player_id("/s/player/6b3d0364/tv-player-ias.vflset/tv-player-ias.js"), Some("6b3d0364"));
        assert_eq!(player_id("https://www.youtube.com/s/player/ab12/x.js"), Some("ab12"));
        assert_eq!(player_id("/s/other.js"), None);
        assert_eq!(signature_timestamp("a,signatureTimestamp:20420,b"), Some(20420));
        assert_eq!(signature_timestamp("nothing"), None);
    }

    /// Network + token.json: the signed-in TV client's URLs, solved, download (206 + an MP4
    /// header); unsolved they are refused. `YTNATIVE_TEST_VIDEO` picks the video (e.g. a private
    /// one of the signed-in account).
    #[test]
    #[ignore]
    fn live_signed_in_urls_download() {
        crate::paths::set_data_dir("C:/Download/prog/ytnative".into());
        let mut s = crate::session::Session::new().unwrap();
        let bearer = s.bearer().unwrap().expect("no token.json");
        let video = std::env::var("YTNATIVE_TEST_VIDEO").unwrap_or_else(|_| "MlJqlg-N0DY".into());
        let t0 = std::time::Instant::now();
        let r = crate::streams::signed_in(&s.api(), &video, &bearer).unwrap().unwrap();
        eprintln!("resolved {video} ({}) via {:?} in {:.1}s", r.title, r.client, t0.elapsed().as_secs_f32());
        assert!(!r.title.is_empty() && !r.author.is_empty(), "title / author missing");
        // As the TV's media pipeline fetches them: right away, with its own user agent.
        let ua = "Mozilla/5.0 (Web0S; Linux/SmartTV) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/94.0 Safari/537.36";
        let mut checked = 0;
        let mp4 = |f: &Value| f["mimeType"].as_str().is_some_and(|m| m.contains("avc1") || m.contains("mp4a"));
        for f in r.adaptive.iter().filter(|f| mp4(f)) {
            let url = f["url"].as_str().expect("format without url");
            let resp = s.http.get(url).header("User-Agent", ua).header("Range", "bytes=0-1023").send().unwrap();
            let status = resp.status().as_u16();
            eprintln!("itag {} -> {status}", f["itag"]);
            assert_eq!(status, 206, "itag {}", f["itag"]);
            assert_eq!(&resp.bytes().unwrap()[4..8], b"ftyp");
            checked += 1;
        }
        assert!(checked > 0, "no H.264/AAC formats checked");
        assert!(r.mpd(Default::default()).is_some(), "no DASH manifest from the solved formats");
    }

    #[test]
    fn query_params_are_read_and_replaced() {
        let u = "https://x/videoplayback?a=1&n=abc%3D&b=2";
        assert_eq!(query_param(u, "n"), Some("abc%3D"));
        assert_eq!(query_param(u, "nn"), None);
        assert_eq!(set_query_param(u, "n", "zz"), "https://x/videoplayback?a=1&n=zz&b=2");
        assert_eq!(set_query_param("https://x/v?a=1", "sig", "q"), "https://x/v?a=1&sig=q");
        assert_eq!(percent_decode("a%3Db+c%2"), "a=b c%2");
        assert_eq!(percent_encode("a=b/c"), "a%3Db%2Fc");
        let f = form_fields("s=AB%3D%3D&sp=sig&url=https%3A%2F%2Fx%2Fv%3Fn%3D1");
        assert_eq!(f["s"], "AB==");
        assert_eq!(f["url"], "https://x/v?n=1");
    }
}
