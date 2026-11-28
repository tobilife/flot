//! Why a changed step's receipt changed, read from the two receipts' bodies.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::receipt::{FileDigest, ReceiptIdentity};

/// How many files one reason names before it only counts the rest.
const NAMED_FILES: usize = 5;

/// What makes two receipts of one step differ. A different key means the
/// work was not the same: its command, the files it or its proof read, its
/// dependencies' receipts, the flot that derived it, its claims, or the rest
/// of its definition. The same key means the same work came out otherwise.
pub(super) fn why(left: &ReceiptIdentity, right: &ReceiptIdentity) -> Vec<String> {
    let mut why = Vec::new();
    if left.step_key == right.step_key {
        // What matters most first: whether it succeeded and what its claims
        // came to; output that prints a time differs on every run.
        let mut differs = Vec::new();
        if left.exit_code != right.exit_code {
            let code = |code: Option<i32>| code.map_or("none".into(), |code| code.to_string());
            differs.push(format!(
                "exit {} -> {}",
                code(left.exit_code),
                code(right.exit_code)
            ));
        }
        if left.claims != right.claims {
            differs.push("claim results".into());
        }
        if left.outputs != right.outputs {
            differs.push("outputs".into());
        }
        if left.changed != right.changed {
            differs.push("other files it changed".into());
        }
        if left.stdout_digest != right.stdout_digest {
            differs.push("stdout".into());
        }
        if left.stderr_digest != right.stderr_digest {
            differs.push("stderr".into());
        }
        why.push(if differs.is_empty() {
            "same key, and the receipts differ otherwise".into()
        } else {
            format!("same key, different result: {}", differs.join(", "))
        });
        return why;
    }
    if left.command != right.command {
        why.push("command changed".into());
    }
    why.extend(files("inputs", &left.inputs, &right.inputs));
    why.extend(files(
        "proof inputs",
        &left.verify_inputs,
        &right.verify_inputs,
    ));
    let deps = |identity: &ReceiptIdentity| -> BTreeMap<String, String> {
        identity
            .deps
            .iter()
            .map(|dep| (dep.step_id.clone(), dep.receipt_id.clone()))
            .collect()
    };
    let (before, after) = (deps(left), deps(right));
    let changed: Vec<&str> = before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|step| before.get(*step) != after.get(*step))
        .map(String::as_str)
        .collect();
    if !changed.is_empty() {
        why.push(format!("dependencies: {}", changed.join(", ")));
    }
    if left.tool != right.tool {
        why.push(format!(
            "derived by flot {} -> {}",
            left.tool.version, right.tool.version
        ));
    }
    let claimed = |identity: &ReceiptIdentity| -> Vec<(String, Option<String>)> {
        identity
            .claims
            .iter()
            .map(|claim| (claim.text.clone(), claim.verify_command.clone()))
            .collect()
    };
    if claimed(left) != claimed(right) {
        why.push("claims or their proof command changed".into());
    }
    if why.is_empty() {
        why.push("the rest of the step's definition changed (its env, outputs or settings)".into());
    }
    why
}

/// The files of one kind that differ, named up to [`NAMED_FILES`].
fn files(kind: &str, left: &[FileDigest], right: &[FileDigest]) -> Option<String> {
    let index = |files: &[FileDigest]| -> BTreeMap<String, (String, bool)> {
        files
            .iter()
            .map(|file| (file.path.clone(), (file.digest.clone(), file.executable)))
            .collect()
    };
    let (before, after) = (index(left), index(right));
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let differ: Vec<String> = paths
        .into_iter()
        .filter_map(|path| match (before.get(path), after.get(path)) {
            (Some(was), Some(is)) if was != is => Some(format!("{path} changed")),
            (Some(_), None) => Some(format!("{path} removed")),
            (None, Some(_)) => Some(format!("{path} added")),
            _ => None,
        })
        .collect();
    if differ.is_empty() {
        return None;
    }
    let named = differ.len().min(NAMED_FILES);
    let mut text = format!("{kind}: {}", differ[..named].join(", "));
    if differ.len() > named {
        text.push_str(&format!(" and {} more", differ.len() - named));
    }
    Some(text)
}
