//! The starter flow `flot init` writes, with its proofs held to the
//! remote's default branch when there is one.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::engine::git::Git;
use crate::service::Workspace;

/// The flow `flot init` writes, with `{proofs_from}` replaced by
/// [`starter_proofs_from`].
const STARTER_FLOW: &str = r#"# flot flow. Every step records a receipt; every claim needs a proof command.
[flow]
name = "starter"
description = "Replace these steps with your own work."
{proofs_from}budget = { timeout_secs = 900, max_inline_bytes = 4096 }

[[step]]
id = "inspect"
run = "ls -a"
# Declared inputs make a cache hit trustworthy: change them and the step re-runs.
inputs = ["flot.toml"]
claims = ["the flow file is present"]
verify = "test -f flot.toml"

[[step]]
id = "report"
run = "echo 'replace me with real work'"
needs = ["inspect"]
# No claims: this step's success proves nothing, and flot will say so.
"#;

/// The `proofs_from` of a starter flow in `root`: the remote's default
/// branch when the repository names one (`origin/main` after a clone), a
/// revision an agent working here does not control; else a comment saying
/// what to set and why.
fn starter_proofs_from(root: &Path) -> String {
    let remote = Git::open(root)
        .and_then(|git| {
            git.run(
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
                None,
            )
        })
        .ok()
        .filter(|rev| !rev.is_empty() && !rev.starts_with('-'));
    match remote {
        Some(rev) => format!(
            "# Every proof must match {rev}, which an agent working here does not\n# control: a proof changed along with the work stays unverified.\nproofs_from = {}\n",
            toml::Value::String(rev.clone())
        ),
        None => "# Without proofs_from, a proof edited along with the work it judges passes.\n# Name a revision an agent working here does not control, such as the\n# remote's default branch:\n# proofs_from = \"origin/main\"\n"
            .to_string(),
    }
}

impl Workspace {
    /// Write the starter flow. Refuses to replace an existing file unless
    /// `force` is set.
    pub fn init(&self, force: bool) -> Result<PathBuf> {
        if self.flow_path.exists() && !force {
            bail!(
                "{} already exists; pass --force to overwrite",
                self.flow_path.display()
            );
        }
        let flow = STARTER_FLOW.replace("{proofs_from}", &starter_proofs_from(&self.root));
        std::fs::write(&self.flow_path, flow)
            .with_context(|| format!("cannot write {}", self.flow_path.display()))?;
        Ok(self.flow_path.clone())
    }
}
