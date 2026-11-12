# Vendored: plx-native UI core

- Source: https://github.com/GLinnik21/plx-native at commit `31e6f24` (2026-11-12)
- License: GPL-3.0-or-later (see `LICENSE`, `LICENSING.md`, `THIRD-PARTY-NOTICES.md` in this folder).
  Fonts and icons under `assets/` keep their own licenses (OFL-1.1, Apache-2.0, MIT) as listed there.
- Copied: `rust-modules/{base,machine,gfx,platform,ui,build_support}`, `assets/`, `locales/`,
  `ci/install-identities.json`, `src/main.c` (referenced by a test via include_str).
- The PlxNative name and brand artwork are not used by ytnative (see upstream TRADEMARKS.md).
- Local changes are made as separate commits so they can be diffed against upstream.

## Local changes
- `ui/src/lib.rs`: `card_row` made `pub` (apps need `RowStyle` / `TileLabel` for their card sources).
- `platform/src/tv/toast.rs`: `deliver_without_port` made `pub` (used by our `tv::Port`).
