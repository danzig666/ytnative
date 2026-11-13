# textshape: `libass-plx.so.0` for the TV

The text shaper behind the vendored plx `gfx::rtl` module: HarfBuzz shaping, FreeType rasterizing
and FriBidi reordering for Arabic, Hebrew, the Indic scripts and the other complex scripts the
television's SDL2_ttf cannot draw correctly.

- `ass_text.c` is plx-native's `src/ass_text.c` (GPL-3.0-or-later, commit `31e6f24`), with the face
  cache raised from 8 to 32 files and `plx_ass_abi_version` added; `ass.h` is the text half of its
  `include/ass.h`.
- Unlike upstream (which links a pinned FreeType/HarfBuzz/FriBidi statically), this library links
  the television's own `libfreetype.so.6`, `libharfbuzz.so.0` and `libfribidi.so.0`, through the
  link-only stubs from `ci/symbols/` (no LG binary is used to build). The headers in
  `third_party/shaping/include` are upstream FreeType 2.11.1, HarfBuzz 4.0.1 and FriBidi 1.0.11.
- `cargo xtask build` compiles it with zig next to the app binary; the IPK ships it beside the
  binary, where `gfx::rtl` loads it. If it fails to load, the app falls back to unshaped text.
