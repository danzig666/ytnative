//! **A playlist's videos in order**, for the app's play queue.
//!
//! A playlist's own page (browse `VL<id>`) is no good for this: YouTube's TV client sends its
//! first 15 videos and no continuation. The watch page's playlist panel is: asked with the
//! playlist id alone (no video), `next` starts the panel at the playlist's first video, and its
//! continuation walks the list, 20 videos at a time. Past the end the windows start over with the
//! first video (with new tokens), which is where a walk stops. Asked with a video further down,
//! the panel is a window around that video instead (so it cannot be used to load the list).
//! Mixes (ids starting with "RD": "My Mix", "Mix – <artist>") are endless: the panel says
//! `isInfinite` and its continuation keeps giving more.

use crate::model::{Item, Page, Shelf};
use crate::session::Session;
use crate::watch::{parse_watch, PlaylistPanel};
use anyhow::{anyhow, Result};
use serde_json::json;

/// A mix: YouTube makes it up as it plays (endless), so there is no complete list to load.
pub fn is_mix(playlist_id: &str) -> bool {
    playlist_id.starts_with("RD")
}

/// Marks a playlist page's continuation that goes on through the playlist panel
/// ([`Session::playlist_page`]) rather than through `browse`: `<prefix><first video id> <token>`.
/// The first video is what tells a window past the list's end (it starts over with it).
const PANEL_TOKEN: &str = "ytnative-panel:";

/// Whether `continuation` is a playlist page's panel continuation ([`Session::playlist_page_more`]).
pub fn is_panel_token(continuation: &str) -> bool {
    continuation.starts_with(PANEL_TOKEN)
}

fn panel_token(first: &str, token: &str) -> String {
    format!("{PANEL_TOKEN}{first} {token}")
}

fn split_panel_token(continuation: &str) -> Option<(&str, &str)> {
    continuation.strip_prefix(PANEL_TOKEN)?.split_once(' ')
}

/// The playlist page's cards followed by the panel's first window (the videos after the page's
/// 15, without the ones it already has), and the panel's continuation to the rest.
fn with_panel(mut page: Page, panel: PlaylistPanel) -> Page {
    let Some(first) = panel.items.first().map(|i| i.id.clone()) else { return page };
    let have: std::collections::HashSet<String> = page.shelves.iter().flat_map(|s| s.items.iter().map(|i| i.id.clone())).collect();
    let extra: Vec<Item> = panel.items.into_iter().filter(|i| !have.contains(&i.id)).collect();
    match page.shelves.last_mut() {
        Some(sh) => sh.items.extend(extra),
        None => page.shelves.push(Shelf { items: extra, ..Default::default() }),
    }
    page.continuation = panel.continuation.map(|t| panel_token(&first, &t));
    page
}

/// A position in a list of `total` (> 0) from a random number: each equally likely (up to the
/// negligible bias of a 64-bit modulo).
fn random_index(total: usize, rand: u64) -> usize {
    (rand % total as u64) as usize
}

/// A window of the panel as the next page of a playlist page: its videos, and the continuation
/// on, unless this window is the last (empty, no token, the token it was asked with, or past the
/// end: the list's first video again).
fn panel_page(first: &str, asked: &str, items: Vec<Item>, next: Option<String>) -> Page {
    let round = items.iter().any(|i| i.id == first);
    let next = next.filter(|n| !items.is_empty() && !round && n != asked);
    Page {
        shelves: vec![Shelf { items, ..Default::default() }],
        continuation: next.map(|t| panel_token(first, &t)),
        ..Default::default()
    }
}

impl Session {
    /// The first window of a playlist (from its first video) with the continuation to the rest.
    pub fn playlist_start(&mut self, playlist_id: &str) -> Result<PlaylistPanel> {
        let w = parse_watch(&self.raw("next", json!({ "playlistId": playlist_id }))?);
        w.playlist.ok_or_else(|| anyhow!("no playlist panel for {playlist_id}"))
    }

    /// The next window of a playlist panel ([`PlaylistPanel::continuation`]).
    pub fn playlist_more(&mut self, continuation: &str) -> Result<(Vec<Item>, Option<String>)> {
        self.shelf_more("next", continuation)
    }

    /// **A playlist's own page with all its videos.** The page (browse `VL<id>`) stops at 15 with
    /// no continuation, so the rest comes from the playlist panel: its first window is added to
    /// the page, and the page's continuation walks the panel ([`Session::playlist_page_more`]).
    /// A page that has a continuation of its own, a mix, or a panel that cannot be had is the page
    /// as YouTube sends it.
    pub fn playlist_page(&mut self, playlist_id: &str) -> Result<Page> {
        let page = self.browse(&format!("VL{playlist_id}"), None)?;
        if page.continuation.is_some() || is_mix(playlist_id) {
            return Ok(page);
        }
        Ok(match self.playlist_start(playlist_id) {
            Ok(panel) => with_panel(page, panel),
            Err(_) => page,
        })
    }

    /// The video at `index` (0-based) of a playlist: the watch page started at that position of
    /// the list (`playlistIndex`), so it needs no walk to that far.
    pub fn playlist_video_at(&mut self, playlist_id: &str, index: usize) -> Result<String> {
        let w = parse_watch(&self.raw("next", json!({ "playlistId": playlist_id, "playlistIndex": index }))?);
        Some(w.video_id).filter(|v| !v.is_empty()).ok_or_else(|| anyhow!("no video at {index} of {playlist_id}"))
    }

    /// **A shuffle's first video, from the whole playlist**: position `rand % total` of the list
    /// (every video equally likely), not only of the videos loaded so far. From the panel's first
    /// window when it is there, else asked for at that position.
    pub fn playlist_random_video(&mut self, playlist_id: &str, rand: u64) -> Result<String> {
        let panel = self.playlist_start(playlist_id)?;
        let total = panel.total.unwrap_or(panel.items.len()).max(panel.items.len());
        if total == 0 {
            return Err(anyhow!("{playlist_id} has no videos"));
        }
        let k = random_index(total, rand);
        match panel.items.get(k) {
            Some(i) => Ok(i.id.clone()),
            None => self.playlist_video_at(playlist_id, k),
        }
    }

    /// The next videos of a [`Session::playlist_page`] from its panel continuation.
    pub fn playlist_page_more(&mut self, continuation: &str) -> Result<Page> {
        let (first, token) = split_panel_token(continuation).ok_or_else(|| anyhow!("not a playlist panel continuation"))?;
        let (items, next) = self.playlist_more(token)?;
        Ok(panel_page(first, token, items, next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str) -> Item {
        Item { id: id.into(), ..Default::default() }
    }

    fn ids(page: &Page) -> Vec<&str> {
        page.shelves.iter().flat_map(|s| s.items.iter().map(|i| i.id.as_str())).collect()
    }

    /// The page's 15 videos go on with the panel's window, without the ones the page has.
    #[test]
    fn the_page_goes_on_with_the_panel() {
        let page = Page { shelves: vec![Shelf { items: (1..=15).map(|i| video(&format!("v{i}"))).collect(), ..Default::default() }], ..Default::default() };
        let panel = PlaylistPanel { items: (1..=20).map(|i| video(&format!("v{i}"))).collect(), continuation: Some("T1".into()), ..Default::default() };
        let p = with_panel(page, panel);
        assert_eq!(ids(&p), (1..=20).map(|i| format!("v{i}")).collect::<Vec<_>>());
        let c = p.continuation.unwrap();
        assert!(is_panel_token(&c));
        assert_eq!(split_panel_token(&c), Some(("v1", "T1")));
    }

    /// No panel window (or one with no continuation): the page as it was, ending there.
    #[test]
    fn an_empty_panel_leaves_the_page() {
        let page = Page { shelves: vec![Shelf { items: vec![video("v1")], ..Default::default() }], ..Default::default() };
        let p = with_panel(page, PlaylistPanel::default());
        assert_eq!(ids(&p), ["v1"]);
        assert!(p.continuation.is_none());
    }

    /// A window goes on with the next token; the walk ends past the list's end (its first video
    /// again), on an empty window, without a token, or with the token it was asked with.
    #[test]
    fn the_walk_ends_at_the_list_end() {
        let more = panel_page("v1", "T1", vec![video("v21"), video("v22")], Some("T2".into()));
        assert_eq!(ids(&more), ["v21", "v22"]);
        assert_eq!(more.continuation.as_deref().and_then(split_panel_token), Some(("v1", "T2")));
        assert!(panel_page("v1", "T2", vec![video("v49"), video("v50"), video("v1")], Some("T3".into())).continuation.is_none());
        assert!(panel_page("v1", "T2", vec![], Some("T3".into())).continuation.is_none());
        assert!(panel_page("v1", "T2", vec![video("v30")], None).continuation.is_none());
        assert!(panel_page("v1", "T2", vec![video("v30")], Some("T2".into())).continuation.is_none());
    }

    /// A random start is a position inside the list, and every position can come up.
    #[test]
    fn a_random_position_is_inside_the_list() {
        for total in [1, 2, 50, 138] {
            let mut hit = vec![false; total];
            for r in u64::MAX - 3 * total as u64..u64::MAX {
                let k = random_index(total, r);
                assert!(k < total);
                hit[k] = true;
            }
            assert!(hit.iter().all(|h| *h), "every position of {total} can come up");
        }
        assert_eq!(random_index(138, 1234567), 1234567 % 138);
    }

    /// Only the panel's own continuation is one: a browse token is not.
    #[test]
    fn a_browse_token_is_not_a_panel_token() {
        assert!(!is_panel_token("4qmFsgKbARIPRkV3aGF0X3RvX3dhdGNo"));
        assert!(split_panel_token("4qmFsgK").is_none());
    }
}

/// Live checks of what each kind of list YouTube shows does: what its card opens, and whether its
/// videos can be followed in order. Signed in with `token.json` in the repository's root:
/// `cargo test -p yt-core --lib playlist -- --ignored --nocapture --test-threads 1`.
#[cfg(test)]
mod live {
    use crate::model::{Endpoint, Item, ItemKind};
    use crate::session::Session;

    /// The public playlist of the report that started this (80s hard rock, 138 videos).
    const LIST: &str = "PLmXxqSJJq-yVWpRFGImHYZBQTuBGLjG4t";

    fn session() -> Session {
        crate::paths::set_data_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
        Session::new().unwrap()
    }

    fn endpoint(i: &Item) -> String {
        match &i.endpoint {
            Some(Endpoint::Watch { video_id, playlist_id, .. }) => format!("Watch(video {video_id:?}, list {playlist_id:?})"),
            Some(Endpoint::Browse { browse_id, .. }) => format!("Browse({browse_id})"),
            Some(Endpoint::Search { query, .. }) => format!("Search({query})"),
            None => "none".into(),
        }
    }

    /// Every video of a playlist through the panel's continuation (at most `pages` windows), the
    /// way the app walks it: until a window is empty, has no token, names the token it was asked
    /// with, or brings back the list's first video (past its end a finite list starts over).
    /// Returns the videos, whether a token was left, and why the walk ended.
    fn walk(s: &mut Session, id: &str, pages: usize) -> (Vec<String>, bool, &'static str) {
        let p = s.playlist_start(id).unwrap();
        let mut ids: Vec<String> = p.items.iter().map(|i| i.id.clone()).collect();
        let mut token = p.continuation;
        let mut why = "page limit";
        for _ in 0..pages {
            let Some(t) = token.take() else {
                why = "no token";
                break;
            };
            let (more, next) = s.playlist_more(&t).unwrap();
            if more.is_empty() {
                why = "empty window";
                break;
            }
            // Past its end a finite list starts over with new tokens.
            let round = more.iter().any(|i| i.id == ids[0]);
            for i in more {
                if !ids.contains(&i.id) {
                    ids.push(i.id);
                }
            }
            if round {
                why = "went round to the first video";
                break;
            }
            if next.as_deref() == Some(t.as_str()) {
                why = "same token";
                break;
            }
            token = next;
        }
        if why == "page limit" && token.is_none() {
            why = "no token";
        }
        (ids, token.is_some(), why)
    }

    /// What the player saw before (watch page without the playlist id) and sees now (with it).
    fn before_after(s: &mut Session, video: &str, list: &str) {
        let old = s.watch(video, None).unwrap();
        let new = s.watch(video, Some(list)).unwrap();
        let pick = |w: &crate::watch::WatchInfo| w.autoplay_next.as_ref().map(endpoint).unwrap_or_else(|| "none".into());
        println!("  {video} without the list: panel {}, next {}", old.playlist.is_some(), pick(&old));
        match &new.playlist {
            Some(p) => println!(
                "  {video} with the list: panel {:?} ({} videos, at {:?}, endless {}, more {}), next {}",
                p.title,
                p.items.len(),
                p.current_index,
                p.infinite,
                p.continuation.is_some(),
                pick(&new)
            ),
            None => println!("  {video} with the list: no panel, next {}", pick(&new)),
        }
    }

    #[test]
    #[ignore]
    fn playlist_page_and_its_panel() {
        let mut s = session();
        let page = s.browse(&format!("VL{LIST}"), None).unwrap();
        let cards: Vec<&Item> = page.shelves.iter().flat_map(|sh| sh.items.iter()).collect();
        println!("page: {} cards, continuation {}, first {}", cards.len(), page.continuation.is_some(), endpoint(cards[0]));
        before_after(&mut s, &cards[0].id, LIST);
        let (ids, more, why) = walk(&mut s, LIST, 50);
        println!("walked: {} videos, more left {more} (ended: {why}); starts {:?}", ids.len(), &ids[..3]);
        assert!(ids.len() > cards.len() && ids[..cards.len()].iter().zip(&cards).all(|(a, c)| *a == c.id), "the page's cards are the list's start");
        let w = s.watch(&ids[45], Some(LIST)).unwrap();
        let p = w.playlist.unwrap();
        println!("panel at the 46th video starts at {} (a window around it)", p.items.iter().position(|i| i.id == ids[40]).map_or("?".into(), |i| i.to_string()));
    }

    /// The playlist page with every video: the page's 15, then the panel's windows until the end,
    /// the way the app pages it (`Session::playlist_page`, `playlist_page_more`).
    #[test]
    #[ignore]
    fn playlist_page_has_every_video() {
        let mut s = session();
        let mut page = s.playlist_page(LIST).unwrap();
        let mut ids: Vec<String> = Vec::new();
        let mut pages = 1;
        loop {
            for i in page.shelves.iter().flat_map(|sh| sh.items.iter()) {
                if !ids.contains(&i.id) {
                    ids.push(i.id.clone());
                }
            }
            let Some(c) = page.continuation.take() else { break };
            assert!(pages < 40, "the walk did not end");
            page = s.playlist_page_more(&c).unwrap();
            pages += 1;
        }
        let (walked, _, why) = walk(&mut s, LIST, 50);
        println!("page: {} videos in {pages} pages; the panel walk: {} ({why})", ids.len(), walked.len());
        assert!(ids.len() > 15, "more than the page's first 15");
        assert_eq!(ids, walked, "the same videos in the same order as the play queue");
    }

    /// The video at a position far down the list comes straight from `playlistIndex`, the panel
    /// says how long the list is, and a random start can land on any video of it.
    #[test]
    #[ignore]
    fn a_shuffle_can_start_anywhere() {
        let mut s = session();
        let (walked, _, _) = walk(&mut s, LIST, 50);
        assert_eq!(s.playlist_start(LIST).unwrap().total, Some(walked.len()), "the panel's totalVideos");
        for k in [0, 19, 20, 100, walked.len() - 1] {
            assert_eq!(s.playlist_video_at(LIST, k).unwrap(), walked[k], "video at {k}");
        }
        let v = s.playlist_random_video(LIST, 1234567).unwrap();
        let at = walked.iter().position(|x| *x == v);
        println!("random start: {v} at {at:?} of {}", walked.len());
        assert_eq!(at, Some(1234567 % walked.len()));
    }
    #[test]
    #[ignore]
    fn watch_later_and_liked() {
        let mut s = session();
        for id in ["WL", "LL"] {
            match s.playlist_start(id) {
                Ok(p) => println!("{id}: {:?}, {} videos, endless {}, more {}", p.title, p.items.len(), p.infinite, p.continuation.is_some()),
                Err(e) => println!("{id}: {e:#}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn mixes() {
        let mut s = session();
        let mut cards: Vec<Item> = Vec::new();
        for sh in s.home().unwrap().shelves {
            cards.extend(sh.items.into_iter().filter(|i| i.kind == ItemKind::Mix));
        }
        println!("home: {} mix cards", cards.len());
        let found = s.search("lofi mix", None).unwrap();
        cards.extend(found.shelves.into_iter().flat_map(|sh| sh.items).filter(|i| i.kind == ItemKind::Mix).take(2));
        let w = s.watch("SwYN7mTi6HM", None).unwrap();
        cards.extend(w.suggestions.into_iter().flat_map(|sh| sh.items).filter(|i| i.kind == ItemKind::Mix).take(2));
        assert!(!cards.is_empty());
        for c in cards.iter().take(5) {
            println!("{:?} {:?}: {}", c.kind, c.title, endpoint(c));
            let Some(Endpoint::Watch { video_id, playlist_id: Some(pl), .. }) = &c.endpoint else { continue };
            assert!(super::is_mix(pl));
            before_after(&mut s, video_id, pl);
            let p = s.watch(video_id, Some(pl)).unwrap().playlist.unwrap();
            let (more, _) = s.playlist_more(p.continuation.as_deref().unwrap()).unwrap();
            println!("  the panel's continuation: {} more, all in the mix {}", more.len(), more.iter().all(|i| matches!(&i.endpoint, Some(Endpoint::Watch { playlist_id: Some(x), .. }) if x == pl)));
        }
    }

    #[test]
    #[ignore]
    fn playlist_cards() {
        let mut s = session();
        let mut cards: Vec<Item> = Vec::new();
        // Search results filtered to playlists, an album search, and a channel's uploads.
        for q in ["80s rock playlist", "back in black album"] {
            let found = s.search(q, Some("EgIQAw%3D%3D")).unwrap();
            cards.extend(found.shelves.into_iter().flat_map(|sh| sh.items).filter(|i| i.kind == ItemKind::Playlist).take(2));
        }
        let w = s.watch("SwYN7mTi6HM", None).unwrap();
        cards.extend(w.suggestions.into_iter().flat_map(|sh| sh.items).filter(|i| i.kind == ItemKind::Playlist).take(2));
        for c in &cards {
            println!("{:?} {} {:?}: {}", c.kind, c.id, c.title, endpoint(c));
            let pl = match &c.endpoint {
                Some(Endpoint::Watch { playlist_id: Some(pl), .. }) => pl.clone(),
                Some(Endpoint::Browse { browse_id, .. }) => browse_id.trim_start_matches("VL").to_string(),
                _ => continue,
            };
            let (ids, more, why) = walk(&mut s, &pl, 60);
            println!("  walked {} videos (more left {more}, ended: {why})", ids.len());
            before_after(&mut s, &ids[0], &pl);
        }
        let uploads = format!("UU{}", &w.channel_id[2..]);
        let (ids, more, why) = walk(&mut s, &uploads, 3);
        println!("uploads {uploads}: walked {} videos (more left {more}, ended: {why})", ids.len());
    }
}
