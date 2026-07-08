// A panel renderer returns a list of line strings.
fn render_panel(id, ctx) {
    [
        "cursor line: " + ctx.cursor_line,
        "cursor col:  " + ctx.cursor_col,
        "chars:       " + ctx.doc_text.len,
    ]
}

fn on_command(id, ctx) {
    [ #{ action: "set_panel", panel: "inspector.panel", lines: [
        "refreshed at line " + ctx.cursor_line
    ] } ]
}
