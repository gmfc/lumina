// A viewer renderer returns a list of line strings, exactly like `render_panel`.
//
// `ctx.path` is always provided; `ctx.text` only because the manifest was granted `fs:read`
// (the host truncates it to 1 MB, so an enormous file can't be turned into unbounded work).
fn render_viewer(id, ctx) {
    let raw = "";
    if "text" in ctx {
        raw = ctx.text;
    }
    let sep = ",";
    if ctx.path.ends_with(".tsv") {
        sep = "\t";
    }

    // First pass: split each record into trimmed cells and measure the column widths.
    let rows = [];
    let widths = [];
    for line in raw.split("\n") {
        let cells = cells_of(line, sep);
        if cells.is_empty() {
            continue;
        }
        for i in 0..cells.len {
            let w = cells[i].len;
            if i >= widths.len {
                widths.push(w);
            } else if w > widths[i] {
                widths[i] = w;
            }
        }
        rows.push(cells);
        if rows.len >= 5000 {
            break;      // bounded: a viewer runs on the UI thread
        }
    }

    // Second pass: pad every cell out to its column's width.
    let out = [];
    for cells in rows {
        out.push(pad_row(cells, widths));
    }
    if out.is_empty() {
        out.push("(no rows)");
    }
    out
}

// One record split into trimmed cells; an empty line yields no cells.
fn cells_of(line, sep) {
    let cells = [];
    for cell in line.split(sep) {
        let c = cell;
        c.trim();
        cells.push(c);
    }
    if cells.len == 1 && cells[0] == "" {
        cells.clear();
    }
    cells
}

// Join `cells` with each one padded to its column width (two spaces of gutter).
fn pad_row(cells, widths) {
    let line = "";
    for i in 0..cells.len {
        line += cells[i];
        if i + 1 < cells.len {
            let pad = widths[i] - cells[i].len + 2;
            for _n in 0..pad {
                line += " ";
            }
        }
    }
    line
}
