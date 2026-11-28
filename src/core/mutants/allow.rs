//! The allow list: surviving mutants a person accepts, each with a reason,
//! and the survivors it leaves.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::Survivor;

/// A surviving mutant a person accepts, and why.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowed {
    pub file: String,
    pub mutation: String,
    pub code: String,
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowFile {
    #[serde(default)]
    allow: Vec<Allowed>,
}

/// Read an allow list: `[[allow]]` entries, each with a reason.
pub fn read_allow(text: &str) -> Result<Vec<Allowed>> {
    let file: AllowFile = toml::from_str(text).context("the allow list is malformed")?;
    for entry in &file.allow {
        if entry.reason.trim().is_empty() {
            bail!(
                "the allow list's entry for `{}` in {} gives no reason",
                entry.mutation,
                entry.file
            );
        }
    }
    Ok(file.allow)
}

/// Split the survivors into those no entry allows and those one does.
pub fn judge(survived: Vec<Survivor>, allowed: &[Allowed]) -> (Vec<Survivor>, Vec<Survivor>) {
    survived.into_iter().partition(|survivor| {
        !allowed.iter().any(|entry| {
            entry.file == survivor.file
                && entry.mutation == survivor.mutation
                && entry.code == survivor.code
        })
    })
}
