//! The built-in language-server registry: the knowledge that lets the editor "just work" with
//! zero config. Maps a language id to a priority-ordered list of server launch candidates plus a
//! one-line install hint. Discovery is lazy and pure at the edges: [`first_installed`] and
//! [`exe_in_dirs`] take their probe/paths as arguments so the resolution logic is unit-testable
//! without touching the process environment. The `LspManager` consults this only when discovery is
//! enabled and no explicit `[lsp]` override exists (override always wins — §10, user config).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One known language server: how to launch it, and how to install it if it is missing.
pub(crate) struct ServerDef {
    /// argv candidates in priority order; the first whose program resolves on `$PATH` wins.
    pub(crate) candidates: &'static [&'static [&'static str]],
    /// One-line, copy-paste install hint surfaced when no candidate resolves (read via
    /// `install_hint`, which the LSP panel shows on a `NotInstalled` row).
    install: &'static str,
}

/// The language id → server table. The single source of zero-config discovery; keys must match the
/// ids produced by [`crate::files::language_for`]. Adding a language is a one-row change.
pub(crate) fn registry() -> &'static HashMap<&'static str, ServerDef> {
    static REG: OnceLock<HashMap<&'static str, ServerDef>> = OnceLock::new();
    REG.get_or_init(build_registry)
}

/// The install hint for a language, if it has a known server.
pub(crate) fn install_hint(lang: &str) -> Option<&'static str> {
    registry().get(lang).map(|d| d.install)
}

fn build_registry() -> HashMap<&'static str, ServerDef> {
    // A tiny helper keeps the table below dense and readable.
    fn def(candidates: &'static [&'static [&'static str]], install: &'static str) -> ServerDef {
        ServerDef {
            candidates,
            install,
        }
    }
    // The TS/JS servers are shared (typescript-language-server serves both); list vtsls first as
    // the modern default, then the classic server.
    const TS_JS: &[&[&str]] = &[
        &["vtsls", "--stdio"],
        &["typescript-language-server", "--stdio"],
    ];
    const TS_JS_INSTALL: &str = "npm i -g @vtsls/language-server (or typescript-language-server)";
    // The vscode-langservers-extracted bundle ships json/html/css servers together.
    const VSCODE_INSTALL: &str = "npm i -g vscode-langservers-extracted";

    HashMap::from([
        (
            "rust",
            def(&[&["rust-analyzer"]], "rustup component add rust-analyzer"),
        ),
        ("typescript", def(TS_JS, TS_JS_INSTALL)),
        ("javascript", def(TS_JS, TS_JS_INSTALL)),
        (
            "python",
            def(
                &[&["pyright-langserver", "--stdio"], &["pylsp"]],
                "npm i -g pyright (or pipx install python-lsp-server)",
            ),
        ),
        (
            "go",
            def(&[&["gopls"]], "go install golang.org/x/tools/gopls@latest"),
        ),
        (
            "c",
            def(&[&["clangd"]], "install clangd (LLVM / distro package)"),
        ),
        (
            "cpp",
            def(&[&["clangd"]], "install clangd (LLVM / distro package)"),
        ),
        ("java", def(&[&["jdtls"]], "install jdtls (eclipse.jdt.ls)")),
        (
            "kotlin",
            def(
                &[&["kotlin-language-server"]],
                "brew install kotlin-language-server",
            ),
        ),
        (
            "ruby",
            def(
                &[&["ruby-lsp"], &["solargraph", "stdio"]],
                "gem install ruby-lsp",
            ),
        ),
        (
            "php",
            def(
                &[
                    &["intelephense", "--stdio"],
                    &["phpactor", "language-server"],
                ],
                "npm i -g intelephense",
            ),
        ),
        (
            "csharp",
            def(&[&["csharp-ls"]], "dotnet tool install -g csharp-ls"),
        ),
        (
            "lua",
            def(
                &[&["lua-language-server"]],
                "brew install lua-language-server",
            ),
        ),
        (
            "bash",
            def(
                &[&["bash-language-server", "start"]],
                "npm i -g bash-language-server",
            ),
        ),
        (
            "yaml",
            def(
                &[&["yaml-language-server", "--stdio"]],
                "npm i -g yaml-language-server",
            ),
        ),
        (
            "html",
            def(
                &[&["vscode-html-language-server", "--stdio"]],
                VSCODE_INSTALL,
            ),
        ),
        (
            "css",
            def(
                &[&["vscode-css-language-server", "--stdio"]],
                VSCODE_INSTALL,
            ),
        ),
        (
            "scss",
            def(
                &[&["vscode-css-language-server", "--stdio"]],
                VSCODE_INSTALL,
            ),
        ),
        (
            "json",
            def(
                &[&["vscode-json-language-server", "--stdio"]],
                VSCODE_INSTALL,
            ),
        ),
        (
            "toml",
            def(
                &[&["taplo", "lsp", "stdio"]],
                "cargo install taplo-cli --features lsp",
            ),
        ),
        (
            "sql",
            def(
                &[&["sqls"]],
                "go install github.com/sqls-server/sqls@latest",
            ),
        ),
        (
            "swift",
            def(&[&["sourcekit-lsp"]], "install the Swift toolchain / Xcode"),
        ),
        ("zig", def(&[&["zls"]], "install zls (zigtools/zls)")),
        ("markdown", def(&[&["marksman"]], "brew install marksman")),
    ])
}

/// The first argv candidate whose program passes `installed`, resolved into an owned command.
/// Pure: the probe is injected so the priority logic is testable without a real `$PATH`.
pub(crate) fn first_installed(
    candidates: &[&[&str]],
    installed: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    candidates
        .iter()
        .find(|argv| argv.first().is_some_and(|program| installed(program)))
        .map(|argv| argv.iter().map(|s| s.to_string()).collect())
}

/// Whether `program` is runnable: a path with separators is checked directly; a bare name is
/// looked up across the process `$PATH`.
pub(crate) fn program_on_path(program: &str) -> bool {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    exe_in_dirs(program, &dirs)
}

/// Pure `$PATH`-resolution core: does `program` resolve as an executable, given the search `dirs`?
/// A program containing a path separator (or absolute) is checked in place; otherwise each dir is
/// probed. Split out from [`program_on_path`] so tests supply their own dirs.
fn exe_in_dirs(program: &str, dirs: &[PathBuf]) -> bool {
    let p = Path::new(program);
    if p.is_absolute() || program.contains(std::path::MAIN_SEPARATOR) {
        return is_executable_file(p);
    }
    dirs.iter()
        .any(|dir| is_executable_file(&dir.join(program)))
}

/// Whether `p` is a regular file the OS would run: on Unix that means an execute bit is set; on
/// other platforms we accept a plain file (optionally with a Windows executable extension).
fn is_executable_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(p) {
            Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        p.is_file()
            || ["exe", "cmd", "bat"]
                .iter()
                .any(|e| p.with_extension(e).is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_keys_match_language_ids() {
        // Every registry key must be a language id `language_for` can actually produce, else the
        // server can never be reached. Spot-check the load-bearing ones plus the flagged additions.
        for lang in [
            "rust",
            "typescript",
            "javascript",
            "python",
            "go",
            "c",
            "cpp",
        ] {
            assert!(
                registry().contains_key(lang),
                "missing registry entry for {lang}"
            );
        }
        assert_eq!(registry()["rust"].candidates.len(), 1);
        assert_eq!(registry()["rust"].candidates[0][0], "rust-analyzer");
        assert!(install_hint("rust").unwrap().contains("rustup"));
        assert!(install_hint("nonesuch").is_none());
    }

    #[test]
    fn first_installed_takes_the_highest_priority_present() {
        let candidates: &[&[&str]] = &[
            &["vtsls", "--stdio"],
            &["typescript-language-server", "--stdio"],
        ];
        // Only the second candidate is installed → it wins, args preserved.
        let got = first_installed(candidates, |p| p == "typescript-language-server");
        assert_eq!(
            got,
            Some(vec!["typescript-language-server".into(), "--stdio".into()])
        );
        // The first takes priority when both are present.
        let both = first_installed(candidates, |_| true);
        assert_eq!(both, Some(vec!["vtsls".into(), "--stdio".into()]));
        // None installed → nothing resolves.
        assert_eq!(first_installed(candidates, |_| false), None);
    }

    #[test]
    fn exe_in_dirs_finds_an_executable_by_name() {
        let dir = std::env::temp_dir().join(format!("lumina_reg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("faux-lsp");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dirs = vec![dir.clone()];
        assert!(
            exe_in_dirs("faux-lsp", &dirs),
            "a name present in a search dir resolves"
        );
        assert!(
            !exe_in_dirs("absent-lsp", &dirs),
            "a name absent everywhere does not"
        );
        // An absolute path is checked in place, ignoring the search dirs.
        assert!(exe_in_dirs(&bin.to_string_lossy(), &[]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn exe_in_dirs_rejects_a_non_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lumina_reg_noexec_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("data");
        std::fs::write(&bin, b"not a program").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            !exe_in_dirs("data", std::slice::from_ref(&dir)),
            "a file with no execute bit is not runnable"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
