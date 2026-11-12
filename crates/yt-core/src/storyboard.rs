//! Seek-bar preview thumbnails ("storyboards"): sprite sheets of small frames.
//!
//! Spec (player response `storyboards.playerStoryboardSpecRenderer.spec`):
//! `url_template|w#h#count#cols#rows#interval_ms#name#sigh|...`, one section per level. In the
//! template `$L` is the level index, `$N` the level's name and, inside the name, `$M` the sheet
//! number; the level's `sigh` goes into a `sigh` query parameter. A level with interval 0 spreads
//! its `count` frames evenly over the video. Same as SmartTube's YouTubeStoryParser.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level {
    pub index: usize,
    /// Size of one frame.
    pub width: u32,
    pub height: u32,
    /// Total frames across all sheets.
    pub count: u32,
    pub cols: u32,
    pub rows: u32,
    /// Time between frames; 0 = spread `count` frames over the whole video.
    pub interval_ms: u64,
    /// URL with `$M` still to fill in (sheet number).
    template: String,
}

/// One frame: the sheet image to fetch and the frame's rectangle inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub sheet_url: String,
    pub sheet: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Level {
    pub fn per_sheet(&self) -> u32 {
        (self.cols * self.rows).max(1)
    }

    pub fn sheet_count(&self) -> u32 {
        self.count.div_ceil(self.per_sheet())
    }

    pub fn sheet_url(&self, sheet: u32) -> String {
        self.template.replace("$M", &sheet.to_string())
    }

    /// Effective time per frame for a video of `duration_ms`.
    pub fn frame_ms(&self, duration_ms: u64) -> u64 {
        if self.interval_ms > 0 {
            self.interval_ms
        } else {
            duration_ms.div_ceil(self.count.max(1) as u64).max(1)
        }
    }
}

/// Parses a storyboard spec into its levels (smallest first, as YouTube lists them). Returns an
/// empty list for live-stream specs (`url#w#h#cols#rows`), which need the live segment number.
pub fn parse(spec: &str) -> Vec<Level> {
    let mut sections = spec.split('|');
    let Some(base) = sections.next() else { return Vec::new() };
    let base = base.replace('\\', "");
    let mut out = Vec::new();
    for (i, sec) in sections.enumerate() {
        let f: Vec<&str> = sec.split('#').collect();
        if f.len() < 8 {
            continue;
        }
        let num = |s: &str| s.parse::<u64>().ok();
        let (Some(w), Some(h), Some(count), Some(cols), Some(rows), Some(interval)) =
            (num(f[0]), num(f[1]), num(f[2]), num(f[3]), num(f[4]), num(f[5]))
        else {
            continue;
        };
        // Keep anything after the name as the signature, should it ever contain a #.
        let name = f[6];
        let sigh = f[7..].join("#");
        let mut template = base.replace("$L", &i.to_string()).replace("$N", name);
        if !sigh.is_empty() {
            template.push(if template.contains('?') { '&' } else { '?' });
            template.push_str("sigh=");
            template.push_str(&sigh);
        }
        out.push(Level {
            index: i,
            width: w as u32,
            height: h as u32,
            count: count as u32,
            cols: cols.max(1) as u32,
            rows: rows.max(1) as u32,
            interval_ms: interval,
            template,
        });
    }
    out
}

/// The level to use for previews drawn `target_width` px wide: the smallest at least that wide,
pub fn best_level(levels: &[Level], target_width: u32) -> Option<&Level> {
    levels
        .iter()
        .filter(|l| l.width >= target_width)
        .min_by_key(|l| (l.width, std::cmp::Reverse(l.count)))
        .or_else(|| levels.iter().max_by_key(|l| (l.width, l.count)))
}

/// The frame shown at `pos_ms` of a `duration_ms` video.
pub fn frame_at(level: &Level, pos_ms: u64, duration_ms: u64) -> Frame {
    let idx = (pos_ms / level.frame_ms(duration_ms)).min(level.count.saturating_sub(1) as u64) as u32;
    let sheet = idx / level.per_sheet();
    let cell = idx % level.per_sheet();
    Frame {
        sheet_url: level.sheet_url(sheet),
        sheet,
        x: (cell % level.cols) * level.width,
        y: (cell / level.cols) * level.height,
        w: level.width,
        h: level.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "https://i.ytimg.com/sb/dQw4w9WgXcQ/storyboard3_L$L/$N.jpg?sqp=abc|48#27#100#10#10#0#default#rs$AAA|80#45#108#10#10#2000#M$M#rs$BBB|160#90#108#5#5#2000#M$M#rs$CCC";

    #[test]
    fn parses_levels() {
        let ls = parse(SPEC);
        assert_eq!(ls.len(), 3);
        assert_eq!(ls[0].sheet_url(0), "https://i.ytimg.com/sb/dQw4w9WgXcQ/storyboard3_L0/default.jpg?sqp=abc&sigh=rs$AAA");
        assert_eq!(ls[2].sheet_url(3), "https://i.ytimg.com/sb/dQw4w9WgXcQ/storyboard3_L2/M3.jpg?sqp=abc&sigh=rs$CCC");
        assert_eq!(ls[2].sheet_count(), 5);
        assert_eq!(best_level(&ls, 120).unwrap().index, 2);
        assert_eq!(best_level(&ls, 60).unwrap().index, 1);
        assert_eq!(best_level(&ls, 400).unwrap().index, 2);
        assert!(parse("https://x/$N.jpg#48#27#5#5").is_empty());
    }

    #[test]
    fn frames() {
        let ls = parse(SPEC);
        // Level 2: 2 s per frame, 25 per sheet. 61 s -> frame 30 -> sheet 1, cell 5 -> (0, 1).
        let f = frame_at(&ls[2], 61_000, 213_000);
        assert_eq!((f.sheet, f.x, f.y, f.w, f.h), (1, 0, 90, 160, 90));
        assert!(f.sheet_url.contains("/M1.jpg"));
        // Past the end clamps to the last frame (107 -> sheet 4, cell 7).
        let f = frame_at(&ls[2], 10_000_000, 213_000);
        assert_eq!((f.sheet, f.x, f.y), (4, 2 * 160, 90));
        // Level 0: 100 frames over 200 s -> 2 s each; 199 s -> frame 99 -> (9, 9).
        let f = frame_at(&ls[0], 199_000, 200_000);
        assert_eq!((f.sheet, f.x, f.y), (0, 9 * 48, 9 * 27));
    }

    #[test]
    #[ignore]
    fn live_storyboard() {
        let http = reqwest::blocking::Client::builder().gzip(true).build().unwrap();
        let cfg = crate::bootstrap::fetch(&http).unwrap();
        let api = crate::innertube::Api { http: &http, cfg: &cfg };
        let r = crate::streams::resolve(&api, "dQw4w9WgXcQ", None).unwrap();
        let levels = parse(r.storyboard_spec.as_deref().expect("spec"));
        let l = best_level(&levels, 160).unwrap();
        let f = frame_at(l, 100_000, r.length_secs * 1000);
        eprintln!("level {} ({}x{}, {} frames) -> {:?}", l.index, l.width, l.height, l.count, f);
        let resp = http.get(&f.sheet_url).send().unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let bytes = resp.bytes().unwrap();
        assert!(bytes.starts_with(&[0xFF, 0xD8]), "not a JPEG");
        eprintln!("sheet {} bytes", bytes.len());
    }
}
