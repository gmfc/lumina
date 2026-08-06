# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0] - 2026-08-06

### Added

- **Soft word wrap** — `Alt+Z`, *View: Toggle Word Wrap*, or `line_wrap = true`. Off by
  default. Wrapping is pure view state: the buffer, its transactions, and every character
  offset are untouched, so a wrapped file saves back byte-for-byte identical. `Up`/`Down` move
  by visual row preserving the goal column; `Home`/`End` snap to the visual row. Current
  limits: no continuation indent, wraps at the pane width only, and inlay hints are not
  drawn while wrap is on. In Vim mode `j`/`k` follow the visual row like `Up`/`Down`,
  while operator-pending motions such as `dj` stay logical — a known deviation from Vim,
  tracked in [#54](https://github.com/gmfc/lumina/issues/54).
- **Large-file handling** — opening a file now probes its size and first 8 KiB before reading
  it. At or over `large_file_mb` (default 8) a file still opens, but syntax highlighting, the
  git gutter, and the language server stay off, and the status bar carries a `LARGE` segment
  for as long as it is open. A 200 MB log opens and scrolls instead of stalling the frame.
- **Binary-file refusals** — over `max_file_size_mb` (default 64) a file opens as a notice tab
  with **Open Anyway** (`file.openAnyway`). Binary files get a tab naming the format and their
  size; those refusals are not overridable, because the bytes cannot round-trip a UTF-8 rope
  and the first save would corrupt the file.
- **File viewers as a plugin contribution** — a plugin declares `[[viewers]]` in `plugin.toml`
  with the extensions it claims and publishes styled rows for its tab. The editor owns the tab,
  the scrolling, and the file-IO policy; there is no port for writing bytes back.
- **Built-in `pdf` viewer** — extracts document text page by page. Finds objects by scanning
  rather than walking the xref, expands `/Type /ObjStm` containers, walks `/Pages`→`/Kids` for
  page order, and decodes through `/ToUnicode` CMaps. Encrypted, scanned, and damaged files
  report why instead of showing noise. Adds one dependency, `flate2` (pure-Rust backend).
- **Built-in `hexview` viewer** — `Ctrl+K Ctrl+H` shows `offset  hex  |ascii|` for any file,
  capped at 256 KiB, which makes every binary refusal actionable.
- **`file.openAsText`** (`Ctrl+K Ctrl+T`) — opens a claimed file as text, so a viewer can claim
  an extension but never hold it hostage.
- **Notification log** — every notice is kept in a bounded scrollback, reachable with
  `Ctrl+K Ctrl+N` or *View: Show Notifications*.
- **Keybinding reference** — `Ctrl+K Ctrl+R` opens a shortcut sheet generated from the keymap
  actually in use, so it includes plugin chords and your `[keys]` overrides and cannot drift.
  The documented VS Code deviations head the sheet.
- **Project-local configuration** — `<project>/.lumina/config.toml` layers over the global
  file, alongside the existing `.lumina/plugins` folder. `[settings]` keys and `[lsp]` entries
  win per key, `[keys]` entries apply last, and `[plugins]` can switch a plugin off but never
  force one on. Both files hot-reload. A malformed tier is skipped with an error naming the
  file rather than dropping every tier beneath it.
- **Conflict resolution** — a file changed on disk under a modified buffer now has two exits:
  *File: Revert File* (`file.reloadFromDisk`, confirmed — it discards your edits and that
  file's undo history) and *File: Keep My Version* (`file.keepMine`).
- **Which-key** — an armed chord prefix such as `Ctrl+K` lists what may follow it in the status
  bar.
- **Word wrap in the Settings tab** — the toggle is now reachable from the UI, not only from
  `Alt+Z` and a hand-edited config file.

### Changed

- **`Ctrl+Q` asks before discarding unsaved work.** `app.quit` previously set the quit flag
  unconditionally, bypassing the dirty-tab guards that closing a tab already applied. Sessions
  restore paths, cursors, and scroll — not buffer contents — so nothing else would have brought
  that work back. Quitting now opens a confirmation naming the files at risk: save all & quit,
  discard & quit, or cancel. `:qa!` still force-quits.
- **Save As confirms an overwrite**, shows the absolute path a relative name resolves to as you
  type it, and reports a missing parent directory in the box instead of failing afterwards.
- **Messages have a severity.** The status message was one string cleared by the next
  keystroke, so a save failure and a save confirmation had the same lifetime. Notices are now
  typed: Info clears on the next keystroke, Warn and Error hold until superseded or dismissed
  with `Esc`, and the bar is tinted by level. Errors name their recovery with the chord
  actually bound to it, and `io::Error` kinds became sentences that name the file.
- **Persistent states have persistent status segments** — `LARGE` and `CONFLICT` sit beside the
  encoding and line-ending segments instead of being announced once through a message that the
  next keystroke clears. A long message no longer pushes that cluster off the bar.
- **The caret diagnostic holds its own status segment**, separated and dimmed, rather than
  sharing one slot with the status message. When the bar is too narrow for both, they share it
  by a stated rule instead of one silently winning.
- **The command palette shows each command's chord** and floats what you actually reach for to
  the top, via a bounded recency bonus deliberately capped below what a tight contiguous match
  scores — habit breaks ties, but typing always wins. Recorded on activation, not on listing.
- **Overlays standardise on `Esc`**, and a picker with no matches says so instead of rendering
  blank.

### Fixed

- **Ordinary source files were refused as binary.** The `ftyp` signature had no box-size guard,
  so any line whose 5th–8th characters spell it — `int ftype;` — was rejected as MP4 media. Six
  four-letter ASCII signatures likewise refused prose beginning with those words.
- **UTF-32LE files were silently mangled.** `FF FE 00 00` is the UTF-32LE BOM and begins with
  the UTF-16LE one, so such a file was admitted as text, decoded into interleaved NULs, and
  would have written that mojibake over the original on the first save.
- **Viewer and notice tabs could go dirty.** Bracketed paste, plugin edits, and the right-click
  menu all reached the placeholder buffer, which then prompted to save over the file it was
  displaying. Guarded at the two real chokepoints.
- **`Ctrl+K` chords were dead on viewer tabs** — the key handler swallowed chord
  continuations, killing Save All among others on exactly the tabs where the hex-view chord
  lives.
- **The file watcher bypassed the size policy.** An external process turning a 4 KB log into a
  3 GB one made the reload read all of it on the UI thread.
- **PDF parser hangs and panics on damaged input** — quadratic error recovery in arrays and
  dictionaries (a megabyte of letters took over 60 seconds), an unbounded `/ToUnicode` CMap, a
  `u32` overflow in the `bfrange` array form, a `/Kids` cycle bounded only at 2^32 visits,
  `1e30 as usize` saturation, and unbounded `RunLengthDecode` chains.
- **The notification tab never showed its first message** — it detected staleness by comparing
  row counts, and the empty and one-message states have the same count.
- A terminal resize during a file probe reported "Open failed" (`EINTR` is now retried); wide
  CJK characters in a viewer ate the character after them; growing a pane could blank a viewer
  tab; and `0`, the documented "no limit", was clamped away before it could take effect.

### Performance

- **Cheaper frames.** Decoration lookup scanned every published decoration for every character
  on the line; it now filters to the line's range once into reused scratch — roughly 24× on
  that loop at 600 decorations. Per-line style buffers are reused across rows rather than
  allocated per line (~2×), and line text is borrowed from the rope instead of copied when the
  line is contiguous (~1.8×).
- **An idle editor stops burning CPU.** The ~60 Hz loop rebuilt the whole frame and re-ran the
  caret and LSP recomputes every tick; it now does that work only when something actually
  changed, with carve-outs so the diagnostics debounce and a crashed server's restart backoff
  still fire while idle.
- **Cheaper keystrokes.** Completion labels are lowercased once per session instead of on every
  keystroke (~1.9× on refilter), server messages move their payloads instead of deep-cloning
  them, event dedup no longer allocates, and single-cursor selection normalization takes a fast
  path.

### Internal

- Six commits splitting functions past SonarCloud's cognitive-complexity threshold, and a fix
  for a stray `.profraw` file failing the coverage job.

## [0.5.0] - 2026-07-20

### Added

- Zero-config LSP: server discovery, a footer status indicator, a tabbed Terminal/LSP dock,
  server logs, and auto-open.
- A plugin-extensible right-click context menu.

### Changed

- The application crate was renamed `editor-app` → `lumina`; the binary stays `lmn`. Versions
  are centralized at the workspace so future bumps are one line.

### Fixed

- Document paths are absolutized and normalized, so a relative launch yields valid `file://`
  URIs.

## [0.1.0] - 2026-07-08

Initial release.

[Unreleased]: https://github.com/gmfc/lumina/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/gmfc/lumina/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/gmfc/lumina/compare/v0.1.0...v0.5.0
[0.1.0]: https://github.com/gmfc/lumina/releases/tag/v0.1.0
