# Lumina Usability Review — Nielsen's 10 Heuristics

_A heuristic evaluation of Lumina's interface against [Jakob Nielsen's 10 usability
heuristics](https://www.nngroup.com/articles/ten-usability-heuristics/) (1994). Nielsen's
heuristics are a **quick expert inspection method**, not a conformance checklist — they are
deliberately broad, and the point of applying them is to surface where an interface makes a
user guess, lose work, or feel unmoored. Every finding below cites the source that produces
the behaviour; nothing here is inferred from the README alone._

**Method.** Each heuristic was walked against the surfaces a user actually touches: the status
bar and tab bar (`crates/app/src/ui/chrome.rs`), modal overlays (`ui/overlays.rs`), the
picker/palette (`ui/pickers.rs`, `builtins/src/palette.rs`), notice + viewer tabs
(`ui/tabview.rs`), the Settings tab (`ui/settings.rs`), and the command/key paths that drive
them (`app/dispatch.rs`, `app/keys.rs`, `app/file_ops.rs`, `app/workers.rs`). Findings are
rated by what they cost the user, not by implementation effort.

---

## Status — what has been fixed

**Every finding below has been addressed.** The findings are kept verbatim as the record of
*why* each change was made; the resolution notes say what was actually built and where it lives.

| Finding | Severity | Status |
|---|---|---|
| U1 · one untyped, transient message slot | MAJOR | ✅ `Notice { text, level }` + a bounded log + `view.notifications` |
| U2 · large-file mode announced once | MAJOR | ✅ persistent `LARGE` status segment |
| U3 · external conflict never explained | MAJOR | ✅ `Warn` notice on the transition + persistent `CONFLICT` segment |
| U4 · internal vocabulary in user text | MINOR | ✅ server named where known; stale-edit + unknown-command rephrased |
| U5 / U10 · `Ctrl+Q` discards unsaved work | **CRITICAL** | ✅ `ConfirmQuit` overlay listing the dirty files |
| U6 · a conflict has no resolution path | MAJOR | ✅ `file.reloadFromDisk` + `file.keepMine` |
| U7 · external reload silently empties undo | MINOR | ✅ the reload says so |
| U8 · three overlay dismissal idioms | MINOR | ✅ `Esc` dismisses everywhere; the hover no longer eats the key |
| U9 · keymap deviations invisible | MINOR | ✅ a "Differs from VS Code" section heads the reference |
| U11 · Save As overwrites silently | MAJOR | ✅ `[O] Overwrite / [Esc] Cancel`, plus a parent-directory check |
| U12 · Save As is a bare text field | MINOR | ✅ the resolved absolute path is shown as you type |
| U13 · palette shows no keybindings | MAJOR | ✅ `CommandInfo::keys` → `PickerItem::hint`, right-aligned in the row |
| U14 · picker has no empty state | MINOR | ✅ `No matching commands` / `No matching files` |
| U15 · chord prefixes hide continuations | MINOR | ✅ which-key list on an armed prefix |
| U16 · no recency/frequency ordering | MINOR | ✅ bounded recency bonus over the fuzzy score |
| U17 · no project-local configuration | MINOR | ✅ `<root>/.lumina/config.toml` layered over the global one |
| U18 · status bar's left slot has four meanings | MINOR | ✅ the diagnostic holds its own segment; levels are colour-coded |
| U19 · errors offer no way out | MAJOR | ✅ every error names its recovery, by live chord |
| U20 · raw `io::Error` text | MINOR | ✅ `ErrorKind` → plain sentences, always naming the file |
| U21 · no in-app help | MAJOR | ✅ `help.keybindings` (generated from the live keymap) + `help.commands` |

The work landed in: `editor.rs` (the notice type, the notice log, the `Text` tab view, the
picker MRU), `app/help.rs` (the reference tabs — new), `app/file_ops.rs` (the quit guard, the
conflict exits, the Save As checks, the error humanizer), `app/overlay.rs` (the new
confirmations), `ui/chrome.rs` (levelled colouring, the `LARGE`/`CONFLICT` segments, the
diagnostic segment, message truncation), `ui/pickers.rs` (hints + the empty state), `picker.rs`
(recency ranking), `config.rs` (the project-local tier), and `app/tests/usability.rs` (new tests,
one per finding). Regressions in behaviour the findings called out as *already strong* are covered
by the existing suite, which still passes.

---

## Executive summary

Lumina's usability is much better than a terminal editor's median, and it is better in the
places that are hardest to get right. The file-open refusal tab (`ui/tabview.rs:42–118`) is a
textbook heuristic-9 artifact: it names the problem, explains it in one plain sentence, and
offers labelled exits whose keys are looked up **live** from the active keymap. The find widget
carries a real match counter and a regex-error slot. The Settings tab renders a description for
the focused row. Degraded modes are deliberate and documented rather than silent failures.

The weaknesses cluster in one structural place: **Lumina has exactly one slot for telling the
user anything, and it is a single `Option<String>` that is cleared at the top of every
dispatch.** `EditorState::status_message` (`editor.rs:127`) is wiped by `dispatch()`
(`app/dispatch.rs:10`) and again by every resolved keystroke (`app/keys.rs:24`). Every message
the editor produces — a save confirmation, a save *failure*, a config parse error, a
"this file opened in degraded mode" notice — has the same lifetime: until the next keypress.
There is no severity, no dwell time, no history, and no way to retrieve a message you blinked
past. Three of the four major findings below are downstream of that one design choice.

The one genuinely serious defect is data loss on quit. `Command::Quit` sets `self.quit = true`
unconditionally (`app/dispatch.rs:14`). `request_close` and `close_all_tabs` both guard dirty
buffers behind a confirm overlay; `Ctrl+Q` bypasses both, and session restore persists only
paths, cursors, and scroll offsets (`app/lifecycle.rs:181–200`) — not buffer contents. Unsaved
work is gone with no prompt, which fails heuristics 3 and 5 in the one way users never forgive.

The second theme is **discoverability of the keymap**. Lumina already has
`Keymap::binding_label(id)` (`keymap.rs:182`), already uses it to show live chords on the
welcome screen and on notice tabs — and does not use it in the command palette, which lists
bare titles (`builtins/src/palette.rs:83`). The palette is the one surface where users learn
shortcuts, and there is no in-app help of any kind to compensate: `commands/tables.rs:5–53` has
no `help.*` entry at all.

---

## Health by heuristic

| # | Heuristic | At review | Now | Note |
|---|---|---|---|---|
| **1** | Visibility of system status | 🟠 **MAJOR GAP** | ✅ | Strong LSP/progress/find indicators, but one transient message slot carries everything; two persistent states (large-file mode, external conflict) are announced once or not at all. *Fixed: notices carry a level and a scrollback; `LARGE` and `CONFLICT` are persistent segments.* |
| **2** | Match between system and real world | 🟡 MINOR DRIFT | ✅ | Palette titles and refusal copy are excellent; raw LSP strings and internal words ("stale edit(s)", "Unknown command") leak into the same bar. *Fixed: the server is named where known, and the internal phrasings were rewritten.* |
| **3** | User control and freedom | 🔴 **CRITICAL** | ✅ | Undo/redo, reopen-closed-tab, and confirm-close are solid — but `Ctrl+Q` discards unsaved buffers with no prompt, and an external-edit conflict has no resolution path. *Fixed: quit is guarded like every other discard path, and a conflict has two named exits.* |
| **4** | Consistency and standards | 🟡 MINOR DRIFT | ✅ | VS Code conventions followed closely and deviations are *documented in code*; but three overlays use three different dismissal idioms, and one dismisses on **any** key. *Fixed: `Esc` dismisses everywhere and the hover no longer swallows the key; the deviations head the in-app reference.* |
| **5** | Error prevention | 🔴 **CRITICAL** | ✅ | Excellent guards where they exist (viewer-tab save guard, reload re-probe, non-overridable binary refusals). Two unguarded destructive paths: quit-with-unsaved, and Save As silently overwriting an existing file. *Fixed: both are confirmed, and Save As vets the path before assigning it.* |
| **6** | Recognition rather than recall | 🟠 **MAJOR GAP** | ✅ | Palette shows no keybindings despite the API existing and being used elsewhere; no empty state; multi-chord prefixes show `Ctrl+K …` without the continuations. *Fixed: all three.* |
| **7** | Flexibility and efficiency of use | ✅ HEALTHY | ✅ | Vim mode, full remapping, palette + quick-open, multi-cursor, session restore, two plugin substrates. *The two gaps named — frecency ordering and project-local config — are now closed (U16, U17).* |
| **8** | Aesthetic and minimalist design | ✅ HEALTHY | ✅ | Genuinely good: responsive welcome screen, width-aware truncation, status bar earns its density. *The one overload (U18) is resolved: the diagnostic holds its own segment and the message slot is colour-coded by level.* |
| **9** | Recognize, diagnose, recover from errors | 🟠 **MAJOR GAP** | ✅ | The notice tab is a model of this heuristic. Everywhere else, errors state the problem and offer no way out — `"Save failed: {e}"` is the whole interaction. *Fixed: `io::Error` kinds became sentences, and each error names its recovery by live chord.* |
| **10** | Help and documentation | 🟠 **MAJOR GAP** | ✅ | No in-app help, no keybinding reference, no `help.*` command. The welcome screen's 13 hints are the only in-app documentation, and they disappear the moment a file is open. *Fixed: `help.keybindings` generates the reference from the live keymap.* |

---

## Findings

### Heuristic 1 — Visibility of system status

**U1 · MAJOR · One transient slot carries every message, regardless of severity.**
`status_message` is a single `Option<String>` (`editor.rs:127`), set to `None` at the top of
`dispatch()` (`app/dispatch.rs:10`) and again on every resolved chord (`app/keys.rs:24`). So
`"Saved /path/to/file"`, `"Save failed: permission denied"`, and
`"Config failed to parse, using defaults: …"` all live exactly one keystroke. A user who saves
and immediately keeps typing never learns the save failed.

> **Fix.** Give the slot a type and a lifetime. Replace `Option<String>` with
> `Option<Notice { text, level: Info|Warn|Error, shown_at: Instant }>`; keep the existing
> clear-on-dispatch for `Info`, hold `Warn`/`Error` until explicitly dismissed or superseded,
> and colour the status bar by level. Add a `view.notifications` command backing a scrollback of
> the last N notices, so nothing is unrecoverable. This is a change in `editor.rs` + one match in
> `ui/chrome.rs:242`; every existing call site keeps compiling behind an `Info` default.

**U2 · MAJOR · Large-file mode is a persistent state announced by a transient message.**
Opening a file at or over `large_file_mb` disables syntax highlighting, the git gutter, and the
LSP, and says so once (`app/file_ops.rs:205–210`). Nothing in `crates/app/src/ui/` reads
`doc.large` — verified by grep. One keystroke later the user is in a mode with no colours, no
gutter, and no diagnostics, and no indication why. (The README's claim that "the status bar says
so" describes the one-shot message, not a persistent indicator.)

> **Fix.** Add a `LARGE` segment to the status bar's right cluster next to the encoding/line-ending
> indicators (`ui/chrome.rs:227–237`), rendered whenever `doc.large`. Persistent state deserves
> persistent display — this is the same argument that already justifies the `LF`/`UTF-8` segments.

**U3 · MAJOR · An external-edit conflict is signalled by a single glyph and never explained.**
When a file changes on disk under a dirty buffer, Lumina correctly refuses to clobber and sets
`doc.external_conflict` (`app/workers.rs:281`). That field is read in exactly one place in the
whole tree: a `⚠` in the tab bar (`ui/chrome.rs:33`), sharing its slot with the `●` dirty marker
and the `↻` reloaded marker. There is no message, no explanation, and no resolution path — see
U6.

> **Fix.** On the transition, publish a `Warn` notice ("`foo.rs` changed on disk; your buffer has
> unsaved changes") and show a persistent `CONFLICT` segment in the status bar while the flag is
> set.

**Already strong:** the LSP health indicator with spinner/ready/error states plus a diagnostic
count badge (`ui/chrome.rs:313–330`), work-done progress with an animated spinner
(`chrome.rs:279–288`), the find widget's `current/total` counter (`builtins/src/find/state.rs:229–235`),
and the viewer tab's `"1–40 of 320"` scroll position (`ui/tabview.rs:153`). These are exactly
right and are the model the findings above ask to be extended.

---

### Heuristic 2 — Match between system and the real world

**U4 · MINOR · Internal vocabulary leaks into user-facing text.** `"LSP: {msg}"` forwards raw
language-server strings verbatim (`app/lsp/events.rs:93`, `:227`); `"Skipped {stale} stale
edit(s)"` (`events.rs:356`) describes an internal revision-mismatch in implementation terms;
`"Unknown command: {other}"` (`app/overlay.rs:28`) is a dispatcher's vocabulary, not a user's.

> **Fix.** Prefix server text with the server's *name* rather than the protocol's acronym
> ("rust-analyzer: …"). Rephrase the stale-edit message in the user's terms ("The file changed
> while the rename was in flight — N edits were skipped"). For an unknown id, say what the user
> can do: "No command `foo` — press Ctrl+Shift+P to browse commands."

**Already strong:** palette titles use the VS Code `Category: Action` convention throughout
(`commands/tables.rs:5–53`), and `ui/tabview.rs:106–118` is the best user-facing copy in the
codebase — *"lumina doesn't display this file as text — its bytes aren't text, and editing them
here would corrupt the file."* That sentence names the problem, the cause, and the consequence
without a single internal term. It is the standard the rest of the messages should be held to.

---

### Heuristic 3 — User control and freedom

**U5 · CRITICAL · `Ctrl+Q` discards unsaved work with no prompt.**
`Command::Quit => self.quit = true` (`app/dispatch.rs:14`) — no dirty check anywhere on the
path. This is inconsistent with Lumina's own behaviour elsewhere: `request_close`
(`app/file_ops.rs:42–56`) opens a confirm overlay for a single dirty tab, and `close_all_tabs`
(`file_ops.rs:126–140`) stops at the first dirty tab specifically so "no unsaved work is lost
silently" — the comment's words. Quit bypasses both. Session restore does not soften this:
`save_session` persists only path, cursor, and scroll (`app/lifecycle.rs:181–200`), so the
buffer contents are genuinely gone.

> **Fix.** Route `app.quit` through the same guard the other paths use. Collect dirty tabs; if
> any exist, open a `ConfirmQuit { dirty: Vec<usize> }` overlay listing them with
> `[S] Save all & quit / [D] Discard & quit / [Esc] Cancel`, mirroring the existing
> `ConfirmClose` box (`ui/overlays.rs:22–47`) so there is nothing new to learn. This is the
> single highest-value change in this document.

**U6 · MAJOR · An external-edit conflict has no resolution path.** Once `external_conflict` is
set there is no command to reload from disk, to diff, or to keep-mine-and-clear. The only exits
are Save — which clobbers the on-disk change — or close-and-discard. The editor detected the
problem correctly and then left the user without a move.

> **Fix.** Add `file.reloadFromDisk` (discard buffer, re-read — undoable is impossible here, so
> confirm it) and `file.keepMine` (clear the flag, accept the fingerprint), both in the palette
> per invariant #4, and surface them from the conflict notice in U3.

**U7 · MINOR · An external reload silently empties undo history.** A clean buffer that changes
on disk is reloaded via `reload_from_str` (`app/workers.rs:300`), which discards undo history —
correct, since the recorded transactions reference stale offsets, but the user is not told. The
next `Ctrl+Z` does nothing, for reasons invisible from the interface.

> **Fix.** Extend the existing `↻` reload path to note it: *"`foo.rs` reloaded from disk — undo
> history for this file was reset."*

**Already strong:** reopen-closed-editor with a stack that skips missing and already-open files
(`file_ops.rs:75–88`), the confirm-close overlay's three clearly-labelled outcomes, total
undo/redo via the transaction engine, and the `Open Anyway` / `Open as Text` escape hatches that
keep a plugin from taking an extension hostage.

---

### Heuristic 4 — Consistency and standards

**U8 · MINOR · Three overlays, three dismissal idioms — one of them dangerous.** In
`app/overlay.rs`: `ConfirmClose` accepts `Esc`/`n`/`c` (`:53`), `SaveAsInput` accepts only `Esc`
(`:63`), and `Info` — the LSP hover box — is dismissed by **any key** (`:59–61`). A user who
reads a hover and resumes typing dismisses it with the first character, which is tolerable; a
user who reaches for a chord gets it swallowed, which is not.

> **Fix.** Standardise on `Esc` as the universal dismiss and let every other key fall through to
> normal handling, so the hover closes *and* the keystroke does what it was going to do.

**U9 · MINOR · Documented keymap deviations are invisible to the user.** The keymap folds Shift
into the char for letter keys, so `ctrl+shift+s` is indistinguishable from `ctrl+s`; Save As is
therefore `Ctrl+K Ctrl+S`, the LSP panel is `Ctrl+K Ctrl+L`, and select-all-matches is `Ctrl+F2`
(`commands/tables.rs:63–66`, `:86–88`). The reasoning is sound and admirably documented *in the
source*. The user discovers it by pressing the VS Code chord and watching Save fire instead.

> **Fix.** Not a code change — a documentation one. A keybinding reference (U21) that marks these
> three rows as "differs from VS Code, and why" converts a surprise into a known quantity.

---

### Heuristic 5 — Error prevention

**U10 · CRITICAL · Quit with unsaved changes.** See U5 — it is an error-prevention failure as
much as a freedom one, and it is the same fix.

**U11 · MAJOR · Save As overwrites an existing file without asking.** `save_as_to`
(`app/file_ops.rs:390–409`) trims the input, resolves it against the workspace root, assigns
`doc.path`, and calls `save_active()`. There is no existence check, no confirmation, and no
validation that the parent directory exists — the only feedback for a bad path is
`"Save failed: {e}"` after the fact.

> **Fix.** Before assigning the path: if it exists, re-open the overlay as a confirm
> (`[O] Overwrite / [Esc] Cancel`); if the parent directory is missing, say so in the prompt's
> `error` slot — the `Prompt` type already carries one (`plugin/src/overlay.rs`), and the find
> widget already uses it for regex errors.

**U12 · MINOR · Save As is a bare text field.** No path completion, no visible expansion of
relative paths against the root, no indication of where the file will land until it lands.

> **Fix.** Render the resolved absolute path under the input as the user types. Completion is a
> larger change; showing the resolution is a few lines in `ui/overlays.rs:75–95` and removes most
> of the guesswork.

**Already strong, and worth naming:** the save guard that refuses to write a notice/viewer tab's
empty placeholder over the real file it is displaying (`file_ops.rs:429–435`) — a genuinely
load-bearing guard against silent destruction. The reload path re-applies the full open policy
before reading, so an external process turning a 4 KB log into a 3 GB one can't freeze the UI
through the back door (`workers.rs:224–241`). And binary refusals are deliberately
*non-overridable* (`files.rs:259`) because those bytes can't round-trip — the right call, and
the notice tab explains it rather than just refusing.

---

### Heuristic 6 — Recognition rather than recall

**U13 · MAJOR · The command palette shows no keybindings.** Commands become picker items as
`PickerItem::new(c.id, c.title)` (`builtins/src/palette.rs:83`), and the renderer draws
`item.label` alone (`ui/pickers.rs:240–243`). Meanwhile `Keymap::binding_label(id)`
(`keymap.rs:182`) resolves a command id to its *live* chord including user overrides — and is
already used by the welcome screen (`ui/chrome.rs:149`) and the notice tab (`ui/tabview.rs:73`).
The palette is where users discover shortcuts in every editor that has one; Lumina has the data
and doesn't show it.

> **Fix.** Add an optional `hint: Option<String>` to `PickerItem`, populate it from
> `binding_label` where the palette builds command items, and right-align it in the picker row.
> The value is disproportionate to the size of the change: it turns the palette from a command
> runner into the keymap teacher, which is most of what U21 is asking for.

**U14 · MINOR · The picker has no empty state.** With zero fuzzy matches, the loop at
`ui/pickers.rs:231–244` emits no rows and the box renders blank below the query line — visually
identical to a picker that is still loading.

> **Fix.** One dim line: `No matching commands` / `No matching files`.

**U15 · MINOR · Multi-chord prefixes show the prefix but not the continuations.** Pressing
`Ctrl+K` sets `"Ctrl+K …"` in the status bar (`app/keys.rs:29`) — good feedback that a chord is
armed, but pure recall for what may follow. `Ctrl+K` has six continuations across the default
bindings.

> **Fix.** The keymap already knows the pending set. Render the completions as a small
> which-key-style list (`Ctrl+S save-as · Ctrl+W close-all · Ctrl+L lsp-panel · …`) when a prefix
> is armed. This is the single cheapest way to make the multi-chord layer learnable.

---

### Heuristic 7 — Flexibility and efficiency of use

Healthy. Vim mode is a full operator+motion+text-object grammar rather than a shortcut list, so
it composes; the keymap is remappable from `[keys]`; the palette and quick-open share one picker
with a `>` mode switch; multi-cursor is the default selection shape rather than a mode; sessions
restore per project root; and plugins can extend the surface through two substrates without
touching the editor.

**U16 · MINOR · No recency or frequency ordering.** `Picker::refilter` ranks by `fuzzy_score`
alone (`picker.rs:115`), so a command used forty times a day sorts identically to one never used.

> **Fix.** Keep a small MRU list of activated ids and add a bounded bonus to the score. Cheap,
> and it compounds for exactly the users who use the palette most.

**U17 · MINOR · No project-local configuration.** `Config::load` reads a single global
`config.toml` from `ProjectDirs` (`config.rs:81`) — while *plugins* already load from a
project-local `.lumina/plugins`. A per-project `tab_width` or LSP mapping has nowhere to live.

> **Fix.** Layer `<root>/.lumina/config.toml` over the global one, matching the precedent the
> plugin loader already sets.

---

### Heuristic 8 — Aesthetic and minimalist design

Healthy, and deliberately so. The welcome screen drops from two columns to one when the pane is
narrow and omits its footer entirely when the height won't take it (`ui/chrome.rs:138–190`); the
status bar's caret-diagnostic text is truncated to the space actually available rather than
wrapping or overflowing (`chrome.rs:250–254`); the viewer header renders through `Paragraph`
specifically so display-width accounting is correct for CJK and emoji (`ui/tabview.rs:120–128`).
That is care, not decoration.

**U18 · MINOR · The status bar's left slot has four meanings and an invisible priority.**
`ui/chrome.rs:242–262` resolves, in order: `status_message`, else the caret diagnostic, else the
file name — then prepends the Vim badge. So moving the caret onto a diagnostic replaces your save
confirmation, and a save confirmation hides the diagnostic you were reading. Both are correct
individually; the user can't tell which rule fired.

> **Fix.** Once notices are typed (U1), give diagnostics their own segment rather than
> time-sharing the message slot — or, more cheaply, prefix each so the source is self-evident.

---

### Heuristic 9 — Help users recognize, diagnose, and recover from errors

**U19 · MAJOR · Errors state the problem and offer no way out.** `"Save failed: {e}"`
(`file_ops.rs:460`), `"Open failed: {e}"` (`:185`, `:282`, `:293`, `:343`), `"No such viewer:
{viewer_id}"` (`:228`). Each names what went wrong; none says what to do. A failed save is the
worst case — the user's work is still unsaved and the interface has moved on, one keystroke
later (U1).

> **Fix.** Pair each error with the recovery that applies: a failed save offers Save As
> (`"Save failed: read-only file — Ctrl+K Ctrl+S to save elsewhere"`); a failed open names the
> path and whether it was permissions or absence; an unknown viewer points at the plugin that
> would provide it. Where a recovery is a command, name its live chord via `binding_label` — the
> notice tab already does exactly this (`ui/tabview.rs:70–88`).

**U20 · MINOR · Raw `io::Error` text reaches the status bar.** `{e}` renders the OS message
("Permission denied (os error 13)"), which is diagnostic for a developer and opaque for a user,
and in the `Open failed` cases the path isn't in the message at all.

> **Fix.** Map the common `ErrorKind`s (`PermissionDenied`, `NotFound`, `IsADirectory`) to plain
> sentences and always include the file name.

**Already strong:** the refusal notice tab is the reference implementation for this heuristic and
should be the template for the fixes above — title, plain-language reason, and only the actions
that actually exist, since `file.openAnyway` is filtered out for non-overridable binary refusals
and `view.openAsHex` disappears with its plugin (`ui/tabview.rs:70–73`). Also good: the config
parse status refuses to lie (`lifecycle.rs:390–403` — *"Config failed to parse, using defaults"*
rather than "reloaded"), the reload refusals explain themselves in the user's terms
(`workers.rs:228–241`), and find surfaces regex errors in a dedicated prompt slot rather than
silently matching nothing.

---

### Heuristic 10 — Help and documentation

**U21 · MAJOR · There is no in-app help.** `palette_entries()` (`commands/tables.rs:5–53`)
contains no `help.*` command. There is no keybinding reference, no way to ask "what is bound to
this key", and no path from the editor to `docs/`. The welcome screen's 13 command hints
(`chrome.rs:76–90`) are the only in-app documentation, and they are the *empty state* — they
vanish the instant a file is open, which is the instant the user starts needing them.

> **Fix.** Add `help.keybindings` ("Help: Keyboard Shortcuts"), opening a read-only tab built
> from the live keymap — the same source `binding_label` reads, so it can never drift from what
> the keys actually do, and it picks up user overrides for free. Render it through the existing
> viewer-tab mechanism so it inherits scrolling and tab management at no cost. Add
> `help.commands` as a thin alias for the palette. Between this and U13, the keymap becomes
> learnable from inside the editor.

**Already strong:** the Settings tab renders the focused row's description plus its key hints in
a footer (`ui/settings.rs:184–191`), with per-setting copy written in user terms
(`crates/app/src/settings.rs:73–204`) — *"At this size a file still opens, but without syntax, git, or LSP."*
That is task-focused, in-context help, and it is the pattern U21 asks to be extended to the
keymap.

---

## Prioritised roadmap

**P0 — data loss and dead ends.** These change outcomes, not impressions. **All delivered.**

| | Finding | Change |
|---|---|---|
| 1 | U5 / U10 | Guard `app.quit` behind a `ConfirmQuit` overlay listing dirty tabs. |
| 2 | U11 | Confirm before Save As overwrites an existing file. |
| 3 | U6 | Add `file.reloadFromDisk` / `file.keepMine` so a conflict has an exit. |

**P1 — the message system.** One change unblocks four findings. **All delivered.**

| | Finding | Change |
|---|---|---|
| 4 | U1 | Type notices (`Info`/`Warn`/`Error`), hold non-info until dismissed, add a notice log. |
| 5 | U2 / U3 | Persistent status-bar segments for large-file mode and conflict state. |
| 6 | U19 / U20 | Pair every error with its recovery, named by live chord; humanise `io::Error`. |

**P2 — discoverability.** Highest ratio of value to diff size in the document. **All delivered.**

| | Finding | Change |
|---|---|---|
| 7 | U13 | Show keybindings in the command palette via the existing `binding_label`. |
| 8 | U21 | `help.keybindings` — a keymap reference tab generated from the live keymap. |
| 9 | U15 | Which-key completions for armed multi-chord prefixes. |
| 10 | U14 / U8 | Picker empty state; standardise overlay dismissal on `Esc`. |

**P3 — polish. All delivered.** U4 (vocabulary), U7 (undo-reset notice), U12 (Save As path
preview), U9 (the keymap deviations head the in-app reference), U16 (a bounded recency bonus over
the fuzzy score, so habit breaks ties without outranking what was typed), U17
(`<root>/.lumina/config.toml` layered over the global file, matching the plugin loader's
precedent), and U18 (the caret diagnostic holds its own segment beside the message rather than
replacing it — the two share the bar's width by a stated rule instead of one silently winning).

---

## A note on the method

Nielsen's heuristics are a lens, not a specification, and two of them are worth reading in
Lumina's favour rather than against it. Heuristic 8's "minimalist" does not mean sparse — the
status bar is dense because a code editor's users want that density, and it is well-ordered.
Heuristic 4's "platform conventions" cuts both ways for a terminal editor that follows VS Code:
the deviations in `commands/tables.rs` are forced by real terminal key-encoding limits, are
documented at the point of definition, and are better surfaced than removed.

The findings above are also weighted toward what the *architecture already supports*. Nearly
every fix listed is either a new command (invariant #4), a new plugin contribution (#3), or a
field on a type that already exists — which is why a document about usability keeps pointing at
`binding_label`, `Prompt::error`, and the notice tab. The affordances are built. In most cases
they are simply not wired to the surface where the user needs them.
