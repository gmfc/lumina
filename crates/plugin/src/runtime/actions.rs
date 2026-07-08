//! Host-action helpers: build transactions against the active document.

use editor_core::Transaction;

use crate::Host;

pub(crate) fn insert_at_cursor(host: &mut dyn Host, text: &str) {
    let Some(id) = host.active_doc() else {
        return;
    };
    let txn = {
        let Some(doc) = host.workspace().documents.get(id) else {
            return;
        };
        let head = doc.selections.primary().head;
        Transaction::insert(doc, head, text)
    };
    host.apply_transaction(id, txn);
}

pub(crate) fn replace_selection(host: &mut dyn Host, text: &str) {
    let Some(id) = host.active_doc() else {
        return;
    };
    let txn = {
        let Some(doc) = host.workspace().documents.get(id) else {
            return;
        };
        let sel = doc.selections.primary();
        Transaction::replace(doc, sel.from()..sel.to(), text)
    };
    host.apply_transaction(id, txn);
}

pub(crate) fn replace_line(host: &mut dyn Host, text: &str) {
    let Some(id) = host.active_doc() else {
        return;
    };
    let txn = {
        let Some(doc) = host.workspace().documents.get(id) else {
            return;
        };
        let head = doc.selections.primary().head;
        let line = doc.char_to_line(head);
        let start = doc.line_to_char(line);
        let end = start + doc.line_len_chars(line);
        Transaction::replace(doc, start..end, text)
    };
    host.apply_transaction(id, txn);
}
