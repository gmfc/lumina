//! Static registration tables: the palette entries and the default keybindings. Kept apart
//! from the `id → Command` resolver so each stays a single, scannable list.

/// User-facing built-in commands shown in the palette: `(id, title)`.
pub fn palette_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("file.save", "File: Save"),
        ("file.saveAs", "File: Save As…"),
        ("file.saveAll", "File: Save All"),
        ("file.new", "File: New File"),
        ("tab.close", "Tab: Close"),
        ("tab.closeAll", "Tab: Close All"),
        ("tab.reopenClosed", "Tab: Reopen Closed Editor"),
        ("tab.next", "Tab: Next"),
        ("tab.prev", "Tab: Previous"),
        ("edit.undo", "Edit: Undo"),
        ("edit.redo", "Edit: Redo"),
        ("edit.selectAll", "Edit: Select All"),
        ("edit.selectWord", "Edit: Select Word"),
        ("edit.selectLine", "Edit: Select Line"),
        ("edit.duplicateLine", "Edit: Copy Line Down"),
        ("edit.copyLineUp", "Edit: Copy Line Up"),
        ("edit.deleteLines", "Edit: Delete Line"),
        ("edit.insertLineBelow", "Edit: Insert Line Below"),
        ("edit.insertLineAbove", "Edit: Insert Line Above"),
        ("edit.moveLineUp", "Edit: Move Line Up"),
        ("edit.moveLineDown", "Edit: Move Line Down"),
        ("edit.toggleComment", "Edit: Toggle Line Comment"),
        (
            "edit.trimTrailingWhitespace",
            "Edit: Trim Trailing Whitespace",
        ),
        ("edit.deleteWordBackward", "Edit: Delete Word Left"),
        // Clipboard copy/cut/paste titles come from the `clipboard` plugin via the registry.
        // All multi-cursor titles (add-next-match / select-all / add-above-below / line-ends) come
        // from the `multicursor` plugin via the registry.
        ("cursor.jumpToBracket", "Go: Jump to Matching Bracket"),
        // All search.* titles (find/replace + project search) come from their plugins.
        // All lsp.* titles come from plugins: hover/goto*/completion/rename/references/symbols from
        // `lsp`, nextDiagnostic/prevDiagnostic from `diagnostics`.
        // git.nextHunk / git.prevHunk titles come from the `git-nav` plugin via the registry
        ("view.toggleSidebar", "View: Toggle Sidebar"),
        // view.toggleTheme title comes from the `theme` plugin; terminal.* from the `terminal` plugin.
        // view.commandPalette / view.quickOpen / view.gotoLine titles come from the `palette` plugin.
        ("view.settings", "Preferences: Open Settings"),
        ("config.reload", "Preferences: Reload Configuration"),
        ("vim.toggle", "Vim: Toggle Vim Mode"),
        ("vim.enable", "Vim: Enable Vim Mode"),
        ("vim.disable", "Vim: Disable Vim Mode"),
        ("app.quit", "Quit"),
    ]
}

/// Default key bindings: `(chord-sequence, command-id)`. Chords are space-separated
/// (VS Code style: `"ctrl+k ctrl+s"`). Config can override or extend these.
pub fn default_bindings() -> &'static [(&'static str, &'static str)] {
    &[
        ("ctrl+q", "app.quit"),
        ("ctrl+s", "file.save"),
        // SPEC-NOTE: the spec suggests `ctrl+shift+s`, but this keymap folds shift into the
        // char for letter keys (keymap.rs), so `ctrl+shift+s` is indistinguishable from
        // `ctrl+s` and would clobber Save. Use a non-colliding multi-chord instead.
        ("ctrl+k ctrl+s", "file.saveAs"),
        ("ctrl+k s", "file.saveAll"),
        ("ctrl+n", "file.new"),
        ("ctrl+w", "tab.close"),
        ("ctrl+k ctrl+w", "tab.closeAll"),
        // `ctrl+shift+t` folds to `ctrl+t` in this keymap (see keymap.rs), which is unbound,
        // so this reaches Reopen Closed Editor exactly as VS Code intends.
        ("ctrl+shift+t", "tab.reopenClosed"),
        ("ctrl+b", "view.toggleSidebar"),
        // ctrl+j / ctrl+` (toggle) + ctrl+pagedown/pageup (next/prev) are contributed by the
        // `terminal` plugin.
        ("ctrl+z", "edit.undo"),
        ("ctrl+y", "edit.redo"),
        ("ctrl+a", "edit.selectAll"),
        // ctrl+c / ctrl+x / ctrl+v (copy/cut/paste) chords are contributed by the `clipboard` plugin.
        // find/replace + project-search (ctrl+shift+f) chords are contributed by their plugins.
        // ctrl+shift+p (palette) + ctrl+p (quick-open) + ctrl+g (goto-line) are contributed by
        // the `palette` plugin.
        ("ctrl+,", "view.settings"),
        // NOTE: `cursor.addNextMatch` (ctrl+d), `cursor.selectAllMatches` (ctrl+f2), and
        // `cursor.addAbove`/`addBelow` (ctrl+alt+up/down) are contributed by the `multicursor`
        // plugin; `git.nextHunk`/`git.prevHunk` (alt+j/alt+k) by `git-nav`. The keymap folds in
        // registry keybindings (build_keymap), so those chords live with their plugins now.
        ("ctrl+tab", "tab.next"),
        ("ctrl+shift+tab", "tab.prev"),
        ("ctrl+1", "tab.goto1"),
        ("ctrl+2", "tab.goto2"),
        ("ctrl+3", "tab.goto3"),
        ("ctrl+4", "tab.goto4"),
        ("ctrl+5", "tab.goto5"),
        ("ctrl+6", "tab.goto6"),
        ("ctrl+7", "tab.goto7"),
        ("ctrl+8", "tab.goto8"),
        ("ctrl+9", "tab.goto9"),
        ("left", "cursor.left"),
        ("right", "cursor.right"),
        ("up", "cursor.up"),
        ("down", "cursor.down"),
        ("home", "cursor.lineStart"),
        ("end", "cursor.lineEnd"),
        ("pageup", "cursor.pageUp"),
        ("pagedown", "cursor.pageDown"),
        ("ctrl+left", "cursor.wordLeft"),
        ("ctrl+right", "cursor.wordRight"),
        ("ctrl+home", "cursor.docStart"),
        ("ctrl+end", "cursor.docEnd"),
        ("shift+left", "select.left"),
        ("shift+right", "select.right"),
        ("shift+up", "select.up"),
        ("shift+down", "select.down"),
        ("shift+home", "select.lineStart"),
        ("shift+end", "select.lineEnd"),
        ("ctrl+shift+left", "select.wordLeft"),
        ("ctrl+shift+right", "select.wordRight"),
        ("enter", "edit.newline"),
        ("backspace", "edit.deleteBackward"),
        ("delete", "edit.deleteForward"),
        ("ctrl+backspace", "edit.deleteWordBackward"),
        ("tab", "edit.indent"),
        ("backtab", "edit.outdent"),
        ("ctrl+/", "edit.toggleComment"),
        ("ctrl+l", "edit.selectLine"),
        ("alt+up", "edit.moveLineUp"),
        ("alt+down", "edit.moveLineDown"),
        ("shift+alt+down", "edit.duplicateLine"),
        ("shift+alt+up", "edit.copyLineUp"),
        // SPEC-NOTE: VS Code's Delete Line is `ctrl+shift+k`, but shift folds into the letter
        // here (keymap.rs), collapsing it to `ctrl+k` — the live chord prefix for Save As /
        // Hover / etc. A `ctrl+k` chord is the non-colliding analogue, mirroring how Save As
        // itself resolves the same collision.
        ("ctrl+k ctrl+k", "edit.deleteLines"),
        ("ctrl+enter", "edit.insertLineBelow"),
        ("ctrl+shift+enter", "edit.insertLineAbove"),
        ("ctrl+k ctrl+x", "edit.trimTrailingWhitespace"),
        ("ctrl+\\", "cursor.jumpToBracket"),
        // shift+alt+i (addCursorsToLineEnds) is contributed by the `multicursor` plugin.
        // f8/shift+f8 (diagnostic nav) are contributed by the `diagnostics` plugin;
        // f12/ctrl+f12/shift+f12/ctrl+shift+o/f2/ctrl+space/ctrl+k ctrl+i (goto* / references /
        // symbols / rename / completion / hover) by the `lsp` plugin.
    ]
}
