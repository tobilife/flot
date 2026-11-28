//! What commands print: lines for a person, safe on a terminal; the
//! Markdown report of `flot verify --markdown`; and pretty JSON.

mod audit;
mod detail;
mod markdown;
mod records;
mod run;
mod setup;
mod share;

use anyhow::Result;
use serde::Serialize;

pub(super) use audit::{render_audit, render_signed};
pub(super) use detail::render_detail;
pub(super) use markdown::markdown_report;
pub(super) use records::{render_diff, render_flaky, render_runs, render_search};
pub(super) use run::{render_limitations, render_plan, render_run};
pub(super) use setup::{render_doctor, render_guard, render_init};
pub(super) use share::{
    render_bundle, render_import, render_key, render_key_rotate, render_pull, render_push,
    render_trust_add, render_trust_list, render_trust_remove, render_trust_rotate,
};

pub(super) fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", json_text(value)?);
    Ok(())
}

/// Pretty JSON that is also safe on a terminal. serde_json escapes only C0
/// controls, so DEL, C1 controls and bidi overrides would reach the terminal
/// raw. Such characters can only occur inside JSON strings, where a `\uXXXX`
/// escape means the same character, so escaping them keeps the document
/// byte-for-byte equivalent to a JSON reader.
fn json_text<T: Serialize>(value: &T) -> Result<String> {
    let text = serde_json::to_string_pretty(value)?;
    if !text.chars().any(needs_escape) {
        return Ok(text);
    }
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if needs_escape(c) {
            escaped.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            escaped.push(c);
        }
    }
    Ok(escaped)
}

/// Characters a terminal could act on, or that make text read differently
/// than it is. Newline and tab are layout, not control.
pub(super) fn needs_escape(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `value` as JSON when the command line asks for `--json`, and otherwise
/// as `human` prints it for a person.
pub(super) fn emit<T: Serialize>(json: bool, value: &T, human: impl FnOnce(&T)) -> Result<()> {
    if json {
        print_json(value)
    } else {
        human(value);
        Ok(())
    }
}
