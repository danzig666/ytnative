//! SABR ("server ABR", `serverAbrStreamingUrl`) media client.
//!
//! YouTube is moving every client to SABR: instead of one URL per format, the player POSTs a
//! protobuf `VideoPlaybackAbrRequest` describing what it holds, and the server answers with a UMP
//! stream of control parts and fMP4/WebM segments. This module is a small blocking implementation:
//!
//! - [`proto`]: hand-written protobuf codec for the needed messages,
//! - [`ump`]: incremental UMP framing parser,
//! - [`formats`]: format selection helpers (best H.264/VP9/AV1 ≤ height, best AAC/Opus),
//! - [`client`]: [`SabrStream`], the session that yields [`Segment`]s.
//!
//! See `client.rs` for the protocol notes; `live.rs` holds the `#[ignore]` live tests.
//!
//! ## Which clients deliver SABR media (measured 2026-11-12, PC, no PO token anywhere)
//!
//! | `/player` client          | auth      | formats w/ URL | SABR media POST                          |
//! |---------------------------|-----------|----------------|------------------------------------------|
//! | VISIONOS                  | anonymous | all            | **200, media**, protection OK; seek to 60 s and to the end OK |
//! | IOS                       | anonymous | all            | 200, media, protection ATTESTATION_PENDING |
//! | ANDROID_VR                | anonymous | all            | 200, media                               |
//! | TVHTML5 7.x               | signed in | none (SABR only) | **HTTP 403** (also with Origin/Referer or Bearer headers) |
//! | TVHTML5 7.x               | anonymous | —              | UNPLAYABLE                               |
//! | VISIONOS/IOS/ANDROID_VR/TVHTML5_SIMPLY | signed in | — | `/player` HTTP 400 (no OAuth support)     |
//! | TVHTML5_SIMPLY(_EMBEDDED_PLAYER) | any | —            | unplayable / "no longer supported"       |
//!
//! So: play through the anonymous VISIONOS response (it carries both direct URLs and a SABR URL),
//! keep the signed-in TV client for metadata. The TV client's SABR endpoint wants a GVS PO token
//! (which we cannot mint); [`SabrStream::set_po_token`] is there for when one is available.
//! NewTube measured IOS/ANDROID_VR returning ATTESTATION_REQUIRED for positions ≥ ~60 s, VISIONOS
//! is not walled.

pub mod client;
pub mod formats;
pub mod proto;
pub mod ump;

#[cfg(test)]
mod live;

pub use client::{SabrConfig, SabrFailure, SabrStats, SabrStream, Segment};
pub use formats::{best_audio, best_video, AudioCodec, SabrFormat, Track, VideoCodec};
pub use proto::ClientInfo;
