//! Previews: the bounded inline view of a step's output a receipt carries,
//! while the complete stream stays in the store. They are written here and
//! checked against the stored stream by [`Observed::describes`].
//!
//! [`Observed::describes`]: crate::core::receipt::Observed::describes

use crate::core::receipt::OMITTED;

/// Bounded inline view of a step's output: its first lines and its last,
/// where builds and test runners report what failed, joined by
/// [`OMITTED`]. Returns the preview and whether anything was left out; the
/// complete stream is always kept in the store.
pub fn stream_preview(data: &[u8], max_bytes: usize) -> (String, bool) {
    if data.len() <= max_bytes {
        return (String::from_utf8_lossy(data).into_owned(), false);
    }
    let Some(room) = max_bytes
        .checked_sub(OMITTED.len())
        .filter(|room| *room >= 64)
    else {
        return inline_preview(data, max_bytes);
    };
    let tail_room = room * 3 / 4;
    let mut head = room - tail_room;
    while head > 0 && (data[head] & 0b1100_0000) == 0b1000_0000 {
        head -= 1;
    }
    // End the beginning at a line break; the marker starts a new line.
    if let Some(newline) = data[..head].iter().rposition(|byte| *byte == b'\n') {
        head = newline;
    }
    let mut tail = data.len() - tail_room;
    while tail < data.len() && (data[tail] & 0b1100_0000) == 0b1000_0000 {
        tail += 1;
    }
    // Start the end on a whole line.
    if let Some(newline) = data[tail..].iter().position(|byte| *byte == b'\n') {
        if tail + newline + 1 < data.len() {
            tail += newline + 1;
        }
    }
    let preview = format!(
        "{}{OMITTED}{}",
        String::from_utf8_lossy(&data[..head]),
        String::from_utf8_lossy(&data[tail..])
    );
    (preview, true)
}

/// The beginning of a stored object, at most `max_bytes`, and whether it was
/// cut there.
pub fn inline_preview(data: &[u8], max_bytes: usize) -> (String, bool) {
    if data.len() <= max_bytes {
        return (String::from_utf8_lossy(data).into_owned(), false);
    }
    let mut cut = max_bytes;
    // Do not split a UTF-8 sequence; walk back to a boundary.
    while cut > 0 && (data[cut] & 0b1100_0000) == 0b1000_0000 {
        cut -= 1;
    }
    (String::from_utf8_lossy(&data[..cut]).into_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_stream_keeps_its_first_and_last_lines() {
        let data: String = (1..=3000).map(|n| format!("line {n}\n")).collect();
        let (text, truncated) = stream_preview(data.as_bytes(), 2048);
        assert!(truncated);
        assert!(text.len() <= 2048, "{}", text.len());
        assert!(text.starts_with("line 1\nline 2\n"), "{text}");
        assert!(text.ends_with("line 2999\nline 3000\n"), "{text}");
        let (head, tail) = text.split_once(OMITTED).unwrap();
        assert!(head.lines().all(|line| line.starts_with("line ")), "{head}");
        assert!(tail.lines().all(|line| line.starts_with("line ")), "{tail}");
        let observed = crate::core::receipt::Observed {
            stdout_inline: text.clone(),
            stdout_truncated: true,
            stdout_bytes: data.len(),
            ..Default::default()
        };
        assert!(observed.describes(data.as_bytes(), b""));
        // A preview whose end was never printed is a forgery.
        let forged = crate::core::receipt::Observed {
            stdout_inline: text.replace("line 3000", "line 9999"),
            ..observed
        };
        assert!(!forged.describes(data.as_bytes(), b""));
    }

    #[test]
    fn a_stream_preview_never_splits_a_character_or_exceeds_its_budget() {
        let data = "한글 ".repeat(2000);
        let (text, truncated) = stream_preview(data.as_bytes(), 500);
        assert!(truncated);
        assert!(text.len() <= 500, "{}", text.len());
        assert!(!text.contains('\u{FFFD}'), "{text}");
        let (head, tail) = text.split_once(OMITTED).unwrap();
        assert!(data.starts_with(head) && data.ends_with(tail));
        // Too small a budget for both ends keeps the beginning.
        let (text, truncated) = stream_preview(data.as_bytes(), 40);
        assert!(truncated);
        assert!(!text.contains(OMITTED) && data.starts_with(&text));
    }

    #[test]
    fn preview_keeps_short_output_whole() {
        let (text, truncated) = inline_preview(b"short", 100);
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    #[test]
    fn preview_cuts_long_output_and_says_so() {
        let (text, truncated) = inline_preview(b"0123456789", 4);
        assert_eq!(text, "0123");
        assert!(truncated);
    }

    #[test]
    fn preview_never_splits_a_multibyte_character() {
        let data = "한글".as_bytes();
        let (text, truncated) = inline_preview(data, 4);
        assert!(truncated);
        assert_eq!(text, "한");
    }
}
