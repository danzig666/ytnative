//! **A playlist's videos in order**, for the app's play queue.
//!
//! A playlist's own page (browse `VL<id>`) is no good for this: YouTube's TV client sends its
//! first 15 videos and no continuation. The watch page's playlist panel is: asked with the
//! playlist id alone (no video), `next` starts the panel at the playlist's first video, and its
//! continuation walks the list to the end, 20 videos at a time. Asked with a video further down,
//! the panel is a window around that video instead (so it cannot be used to load the list).
//! Mixes (ids starting with "RD": "My Mix", "Mix – <artist>") are endless: the panel says
//! `isInfinite` and its continuation keeps giving more.

use crate::model::Item;
use crate::session::Session;
use crate::watch::{parse_watch, PlaylistPanel};
use anyhow::{anyhow, Result};
use serde_json::json;

/// A mix: YouTube makes it up as it plays (endless), so there is no complete list to load.
pub fn is_mix(playlist_id: &str) -> bool {
    playlist_id.starts_with("RD")
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

    /// Every video of a playlist through the panel's continuation (at most `pages` windows).
    fn walk(s: &mut Session, id: &str, pages: usize) -> (Vec<String>, bool) {
        let p = s.playlist_start(id).unwrap();
        let mut ids: Vec<String> = p.items.iter().map(|i| i.id.clone()).collect();
        let mut token = p.continuation;
        for _ in 0..pages {
            let Some(t) = token.take() else { break };
            let (more, next) = s.playlist_more(&t).unwrap();
            let before = ids.len();
            ids.extend(more.into_iter().map(|i| i.id).filter(|v| !ids.contains(v)).collect::<Vec<_>>());
            if ids.len() == before {
                break;
            }
            token = next;
        }
        (ids, token.is_some())
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
        let (ids, more) = walk(&mut s, LIST, 50);
        println!("walked: {} videos, more left {more}; starts {:?}", ids.len(), &ids[..3]);
        assert!(ids.len() > cards.len() && ids[..cards.len()].iter().zip(&cards).all(|(a, c)| *a == c.id), "the page's cards are the list's start");
        let w = s.watch(&ids[45], Some(LIST)).unwrap();
        let p = w.playlist.unwrap();
        println!("panel at the 46th video starts at {} (a window around it)", p.items.iter().position(|i| i.id == ids[40]).map_or("?".into(), |i| i.to_string()));
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
            let (ids, more) = walk(&mut s, &pl, 10);
            println!("  walked {} videos (more left {more})", ids.len());
            before_after(&mut s, &ids[0], &pl);
        }
        let uploads = format!("UU{}", &w.channel_id[2..]);
        let (ids, more) = walk(&mut s, &uploads, 3);
        println!("uploads {uploads}: walked {} videos (more left {more})", ids.len());
    }
}
