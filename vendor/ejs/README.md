yt-dlp's JavaScript challenge solver, release 0.8.0 (https://github.com/yt-dlp/ejs/releases/tag/0.8.0):
`yt.solver.lib.min.js` (meriyah 6.1.4, ISC, and astring 1.9.0, MIT, bundled; their licences are in
its header) and `yt.solver.core.min.js`, both unmodified. Run by `crates/yt-core/src/jsc.rs` on
QuickJS (`vendor/quickjs`). To update: download both files from a newer release and bump
`SOLVER_VERSION` in jsc.rs (cached preprocessed players are then rebuilt).
