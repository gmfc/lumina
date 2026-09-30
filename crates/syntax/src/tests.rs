use super::*;

use editor_core::SyntaxEdit;
use ropey::Rope;

#[test]
fn highlights_rust_keyword_and_string() {
    let src = "fn main() {\n    let s = \"hi\";\n}\n";
    let rope = Rope::from_str(src);
    let mut h = DocHighlighter::new("rust").expect("rust supported");
    h.ensure(&rope, 1, &[], true, 0, rope.len_lines() - 1);
    // Line 0 contains the `fn` keyword.
    let l0 = h.line_spans(0);
    assert!(
        l0.iter().any(|s| s.capture.starts_with("keyword")),
        "expected a keyword on line 0, got {l0:?}"
    );
    // Line 1 contains the string literal "hi".
    let l1 = h.line_spans(1);
    assert!(
        l1.iter().any(|s| s.capture.starts_with("string")),
        "expected a string on line 1, got {l1:?}"
    );
}

#[test]
fn unsupported_language_is_none() {
    assert!(DocHighlighter::new("cobol").is_none());
    assert!(is_supported("rust"));
    assert!(!is_supported("cobol"));
}

/// Every wired grammar must actually load (ABI-compatible with the tree-sitter runtime)
/// and its highlights query must compile — otherwise `DocHighlighter::new` returns None.
#[test]
fn all_wired_grammars_load_and_highlight() {
    let cases = [
        (
            "python",
            "def f():\n    x = \"hi\"\n",
            "line 0 has a keyword",
        ),
        ("javascript", "const x = \"hi\";\n", "line 0 has a keyword"),
        (
            "typescript",
            "const x: number = 1;\n",
            "line 0 has a keyword",
        ),
        ("c", "int main() { return 0; }\n", "line 0 has a keyword"),
        ("go", "package main\n", "line 0 has a keyword"),
        ("toml", "[table]\nkey = 42\n", "line 1 has a number"),
        ("markdown", "# Title\n", "loads"),
        ("html", "<div class=\"x\">hi</div>\n", "loads"),
        ("css", "body { color: red; }\n", "loads"),
        ("java", "class A { int x = 1; }\n", "loads"),
        ("ruby", "def f\n  x = 1\nend\n", "loads"),
        ("bash", "echo hello\n", "loads"),
        ("yaml", "key: value\n", "loads"),
    ];
    for (lang, src, _why) in cases {
        let mut h = DocHighlighter::new(lang)
            .unwrap_or_else(|| panic!("grammar `{lang}` failed to load / compile query"));
        let rope = Rope::from_str(src);
        h.ensure(&rope, 1, &[], true, 0, rope.len_lines() - 1);
        // At least one span somewhere proves the query ran against a real parse tree.
        let any = (0..rope.len_lines()).any(|l| !h.line_spans(l).is_empty());
        assert!(
            any,
            "grammar `{lang}` produced no highlight spans for: {src:?}"
        );
    }
}

#[test]
fn incremental_matches_full_reparse() {
    // Parse once, then apply an incremental edit and confirm the spans match a highlighter
    // that parsed the edited text from scratch.
    let before = "fn main() {\n    let x = 1;\n}\n";
    let after = "fn main() {\n    let yy = 1;\n}\n";

    let mut inc = DocHighlighter::new("rust").unwrap();
    inc.ensure(&Rope::from_str(before), 1, &[], true, 0, 2);

    // Edit: replace "x" (line 1, col bytes 8..9) with "yy".
    let edit = SyntaxEdit {
        start_byte: 20,
        old_end_byte: 21,
        new_end_byte: 22,
        start_point: (1, 8),
        old_end_point: (1, 9),
        new_end_point: (1, 10),
    };
    inc.ensure(&Rope::from_str(after), 2, &[edit], true, 0, 2);

    let mut full = DocHighlighter::new("rust").unwrap();
    full.ensure(&Rope::from_str(after), 1, &[], true, 0, 2);

    for line in 0..3 {
        assert_eq!(
            inc.line_spans(line),
            full.line_spans(line),
            "line {line} spans diverged after incremental edit"
        );
    }
}

#[test]
fn json_highlights_keys_and_numbers() {
    let src = "{\n  \"n\": 42\n}\n";
    let rope = Rope::from_str(src);
    let mut h = DocHighlighter::new("json").unwrap();
    h.ensure(&rope, 1, &[], true, 0, rope.len_lines() - 1);
    let l1 = h.line_spans(1);
    assert!(l1.iter().any(|s| s.capture.starts_with("property")));
    assert!(l1.iter().any(|s| s.capture.starts_with("number")));
}

#[test]
fn user_query_override_is_applied() {
    // A minimal rust query that only captures comments — proves the override path runs.
    let mut map = std::collections::HashMap::new();
    map.insert(
        "rust".to_string(),
        "(line_comment) @comment\n(block_comment) @comment\n".to_string(),
    );
    let mut h = DocHighlighter::new_with_queries("rust", &map).expect("override compiles");
    let rope = Rope::from_str("// hi\nfn main() {}\n");
    h.ensure(&rope, 1, &[], true, 0, rope.len_lines() - 1);
    let l0 = h.line_spans(0);
    assert!(
        l0.iter().any(|s| s.capture.starts_with("comment")),
        "override query should highlight the comment: {l0:?}"
    );
}

#[test]
fn load_user_queries_reads_highlights_scm() {
    let dir = std::env::temp_dir().join(format!("lumina-grammar-test-{}", std::process::id()));
    let lang_dir = dir.join("rust");
    std::fs::create_dir_all(&lang_dir).unwrap();
    std::fs::write(lang_dir.join("highlights.scm"), "(line_comment) @comment\n").unwrap();
    let map = load_user_queries(std::slice::from_ref(&dir));
    assert!(map.get("rust").is_some_and(|s| s.contains("@comment")));
    let _ = std::fs::remove_dir_all(&dir);
}
