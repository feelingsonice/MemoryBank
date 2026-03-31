use std::fmt::Display;
use std::io::{self, Write};

pub(crate) fn no_color_requested() -> bool {
    std::env::var_os("NO_COLOR").is_some()
        || matches!(std::env::var("TERM").ok().as_deref(), Some("dumb"))
}

fn paint(text: &str, code: &str) -> String {
    if no_color_requested() {
        text.to_string()
    } else {
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

pub(crate) fn styled_title(text: &str) -> String {
    paint(text, "1;36")
}

pub(crate) fn styled_command(text: &str) -> String {
    paint(text, "1;36")
}

pub(crate) fn styled_section(text: &str) -> String {
    paint(&format!("== {text} =="), "1;34")
}

pub(crate) fn styled_subtle(text: &str) -> String {
    paint(text, "2")
}

pub(crate) fn styled_success(text: &str) -> String {
    paint(text, "1;32")
}

pub(crate) fn styled_warning(text: &str) -> String {
    paint(text, "1;33")
}

pub(crate) fn styled_failure(text: &str) -> String {
    paint(text, "1;31")
}

pub(crate) fn print_action_start(message: &str) -> io::Result<()> {
    print!("{} ", styled_subtle(message));
    io::stdout().flush()
}

pub(crate) fn print_key_value(label: &str, value: impl Display) {
    println!("  {label}: {value}");
}
