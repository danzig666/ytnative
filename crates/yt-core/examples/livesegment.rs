//! Dev check: demux a saved live fMP4 segment (`cargo run -p yt-core --example livesegment <file>...`).
use yt_core::demux::{mp4::Mp4Demuxer, Demuxer};

fn main() {
    for p in std::env::args().skip(1) {
        let data = std::fs::read(&p).expect("read");
        let mut d = Mp4Demuxer::new();
        d.push(&data).expect("push");
        let (mut n, mut keys, mut first, mut last) = (0, 0, i64::MAX, 0);
        while let Some(au) = d.pop() {
            n += 1;
            keys += au.keyframe as u32;
            first = first.min(au.pts_ns);
            last = last.max(au.pts_ns + au.duration_ns);
        }
        let t = &d.tracks()[0];
        println!("{p}: {:?} {:?} {}x{} {n} units, {keys} key, {:.3}..{:.3} s", t.kind, t.codec, t.width, t.height, first as f64 / 1e9, last as f64 / 1e9);
    }
}
