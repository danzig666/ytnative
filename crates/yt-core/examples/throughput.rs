//! Diagnostic: how fast one format of a video downloads, in the app's 4 MB ranges.
//! cargo run -p yt-core --example throughput -- VIDEO_ID ITAG [MB]
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let (id, itag) = (a[1].clone(), a[2].parse::<u64>()?);
    let mb: u64 = a.get(3).and_then(|v| v.parse().ok()).unwrap_or(24);
    let mut s = yt_core::session::Session::new()?;
    let r = s.resolve(&id)?;
    let f = r.adaptive.iter().find(|f| f["itag"].as_u64() == Some(itag)).ok_or_else(|| anyhow::anyhow!("no itag {itag}"))?;
    let url = f["url"].as_str().ok_or_else(|| anyhow::anyhow!("itag has no url (client {:?})", r.client))?.to_string();
    let bitrate = f["bitrate"].as_u64().unwrap_or(0);
    println!("client {:?} itag {itag} bitrate {:.1} Mbit/s (avg {:.1})", r.client, bitrate as f64 / 1e6, f["averageBitrate"].as_u64().unwrap_or(0) as f64 / 1e6);
    let http = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let ua = r.client.user_agent().to_string();
    let t0 = Instant::now();
    let mut total = 0u64;
    let step = yt_core::demux::ranged::MAX_RANGE;
    let mut start = 0u64;
    while total < mb << 20 {
        let t = Instant::now();
        let b = yt_core::demux::ranged::fetch_range(&http, &url, &ua, start, start + step - 1)?;
        total += b.len() as u64;
        start += b.len() as u64;
        println!("  {} KB in {} ms = {:.1} Mbit/s", b.len() / 1024, t.elapsed().as_millis(), b.len() as f64 * 8.0 / t.elapsed().as_secs_f64() / 1e6);
    }
    let rate = total as f64 * 8.0 / t0.elapsed().as_secs_f64() / 1e6;
    println!("total {} MB in {:.1} s = {:.1} Mbit/s = {:.2}x the bitrate", total >> 20, t0.elapsed().as_secs_f64(), rate, rate * 1e6 / bitrate.max(1) as f64);
    Ok(())
}
