//! What the guard refused, noted one JSON line each for the next run to
//! record.

use std::fs;

use anyhow::{Context, Result};

use super::Store;
use crate::core::receipt::Refusal;

/// What the guard refused, one JSON line each, until a run records it.
const REFUSALS: &str = "refusals.jsonl";

impl Store {
    /// Note an edit the guard refused, for the next run to record. Only in a
    /// store that is already there: a refusal does not create one.
    pub fn note_refusal(&self, refusal: &Refusal) -> Result<()> {
        if !self.root.join("FORMAT").is_file() {
            return Ok(());
        }
        let mut line = serde_json::to_string(refusal)?;
        line.push('\n');
        let path = self.root.join(REFUSALS);
        // One write of one line: notes from hooks running at once do not
        // interleave.
        std::io::Write::write_all(
            &mut fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .with_context(|| format!("cannot open {}", path.display()))?,
            line.as_bytes(),
        )
        .with_context(|| format!("cannot write {}", path.display()))
    }

    /// Refusals noted after `after_ms` and up to `until_ms`, oldest first. A
    /// line that does not parse, such as one cut short, is skipped.
    pub fn refusals(&self, after_ms: u64, until_ms: u64) -> Result<Vec<Refusal>> {
        let path = self.root.join(REFUSALS);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", path.display()))
            }
        };
        let mut found: Vec<Refusal> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Refusal>(line).ok())
            .filter(|refusal| refusal.at_ms > after_ms && refusal.at_ms <= until_ms)
            .collect();
        found.sort_by_key(|refusal| refusal.at_ms);
        Ok(found)
    }
}
