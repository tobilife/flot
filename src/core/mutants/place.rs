//! Where a report places a mutant, and the code that place spans.

use serde::Deserialize;

/// The text from `start` to `end`, 1-based lines and columns, the end
/// excluded; columns count characters.
pub(super) fn span(text: &str, start: (u32, u32), end: (u32, u32)) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = |number: u32| lines.get(usize::try_from(number).ok()?.checked_sub(1)?);
    let from = |line: &str, column: u32| -> Option<usize> {
        let skip = usize::try_from(column).ok()?.checked_sub(1)?;
        if skip == line.chars().count() {
            return Some(line.len());
        }
        line.char_indices().nth(skip).map(|(at, _)| at)
    };
    let (first, last) = (line(start.0)?, line(end.0)?);
    match start.0.cmp(&end.0) {
        std::cmp::Ordering::Greater => None,
        std::cmp::Ordering::Equal => {
            let (a, b) = (from(first, start.1)?, from(first, end.1)?);
            first.get(a..b).map(str::to_string)
        }
        std::cmp::Ordering::Less => {
            let mut code = first.get(from(first, start.1)?..)?.to_string();
            for number in start.0 + 1..end.0 {
                code.push('\n');
                code.push_str(line(number)?);
            }
            code.push('\n');
            code.push_str(last.get(..from(last, end.1)?)?);
            Some(code)
        }
    }
}

#[derive(Deserialize)]
pub(super) struct Position {
    pub(super) line: u32,
    pub(super) column: u32,
}

#[derive(Deserialize)]
pub(super) struct Location {
    pub(super) start: Position,
    pub(super) end: Position,
}
