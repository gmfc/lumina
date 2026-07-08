// A command handler returns a list of *actions*; the host applies only the ones this
// plugin's manifest was granted. The plugin never touches the buffer directly.
fn on_command(id, ctx) {
    if id == "shout.line" {
        [
            #{ action: "replace_line", text: ctx.line_text.to_upper() },
            #{ action: "notify", message: "shouted!" },
        ]
    } else {
        []
    }
}
