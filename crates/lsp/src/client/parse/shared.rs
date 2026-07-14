//! Small parsing helpers shared across the feature parsers (edits, code actions, completion).

use serde_json::Value;

use crate::{Command, TextEdit};

/// Parse a `Command`/`{command, arguments}` object.
pub(super) fn parse_command(v: &Value) -> Option<Command> {
    let command = v.get("command")?.as_str()?.to_string();
    Some(Command {
        command,
        arguments: v.get("arguments").cloned().unwrap_or(Value::Null),
    })
}

pub(super) fn parse_text_edit(v: &Value) -> Option<TextEdit> {
    let range = v.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some(TextEdit {
        start_line: start.get("line")?.as_u64()? as u32,
        start_char16: start.get("character")?.as_u64()? as u32,
        end_line: end.get("line")?.as_u64()? as u32,
        end_char16: end.get("character")?.as_u64()? as u32,
        new_text: v.get("newText")?.as_str()?.to_string(),
    })
}
