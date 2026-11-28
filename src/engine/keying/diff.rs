//! What changed between two digests of the same files, and whether a step
//! was allowed to change it.

use std::collections::BTreeMap;

use crate::core::flow::Step;
use crate::core::receipt::FileDigest;

use super::files::match_options;

/// Paths whose content differs between two digests of the same globs: changed,
/// created or deleted.
pub fn changed_paths(before: &[FileDigest], after: &[FileDigest]) -> Vec<String> {
    let before: BTreeMap<&str, (&str, bool)> = before
        .iter()
        .map(|input| {
            (
                input.path.as_str(),
                (input.digest.as_str(), input.executable),
            )
        })
        .collect();
    let after: BTreeMap<&str, (&str, bool)> = after
        .iter()
        .map(|input| {
            (
                input.path.as_str(),
                (input.digest.as_str(), input.executable),
            )
        })
        .collect();
    let paths: std::collections::BTreeSet<&str> =
        before.keys().chain(after.keys()).copied().collect();
    paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .map(str::to_string)
        .collect()
}

/// Files whose content differs between two snapshots: created or edited,
/// with their content afterwards, or deleted, as `None`. A change of mode
/// alone is not a change of content.
pub fn changes(before: &[FileDigest], after: &[FileDigest]) -> BTreeMap<String, Option<String>> {
    let before: BTreeMap<&str, &str> = before
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    let after: BTreeMap<&str, &str> = after
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .map(|path| {
            (
                path.to_string(),
                after.get(path).map(|digest| digest.to_string()),
            )
        })
        .collect()
}

/// Files created, edited or deleted between two snapshots that `step` may not
/// change: not matched by its `may_change` or its `outputs`.
pub fn out_of_scope(step: &Step, before: &[FileDigest], after: &[FileDigest]) -> Vec<String> {
    let allowed: Vec<glob::Pattern> = step
        .may_change
        .iter()
        .chain(&step.outputs)
        .filter_map(|pattern| glob::Pattern::new(pattern).ok())
        .collect();
    let before: BTreeMap<&str, &str> = before
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    let after: BTreeMap<&str, &str> = after
        .iter()
        .map(|file| (file.path.as_str(), file.digest.as_str()))
        .collect();
    let mut changed: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .filter(|path| {
            !allowed
                .iter()
                .any(|pattern| pattern.matches_with(path, match_options()))
        })
        .map(|path| path.to_string())
        .collect();
    changed.sort();
    changed.dedup();
    changed
}

#[cfg(test)]
mod tests;
