//! Writing the outputs a receipt recorded back into the workspace: the
//! kept candidate's over what is there, a reused receipt's where missing.

use anyhow::{Context, Result};

use crate::core::receipt::Receipt;
use crate::engine::keying;
use crate::engine::runner::Runner;

impl Runner<'_> {
    /// Write the kept candidate's outputs into the workspace, replacing what
    /// is there: they are what its claims were proven against.
    pub(in crate::engine::runner) fn write_back(&self, receipt: &Receipt) -> Vec<String> {
        let mut notes = Vec::new();
        for output in &receipt.identity.outputs {
            let written = (|| -> Result<()> {
                if !keying::within_root(&self.root, &output.path)? {
                    anyhow::bail!("it is reached through a symbolic link");
                }
                let path = self.root.join(&output.path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, self.store.read_bytes(&output.digest)?)?;
                #[cfg(unix)]
                if output.executable {
                    use std::os::unix::fs::PermissionsExt;
                    let mut permissions = std::fs::metadata(&path)?.permissions();
                    permissions.set_mode(permissions.mode() | 0o111);
                    std::fs::set_permissions(&path, permissions)?;
                }
                Ok(())
            })();
            if let Err(error) = written {
                notes.push(format!(
                    "output {} of the kept candidate could not be written back: {error:#}",
                    output.path
                ));
            }
        }
        notes
    }

    /// Put back the outputs of a reused receipt that are missing, so the files
    /// on disk are the ones its claims were proven against. `recall` already
    /// refused any receipt whose outputs exist with other content.
    pub(in crate::engine::runner) fn restore_outputs(
        &self,
        receipt: &Receipt,
    ) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        for output in &receipt.identity.outputs {
            let path = self.root.join(&output.path);
            if path.exists() {
                continue;
            }
            // Checked again here: nothing may be written through a link.
            if !keying::within_root(&self.root, &output.path)? {
                anyhow::bail!(
                    "refusing to restore {}: it is reached through a symbolic link",
                    output.path
                );
            }
            let bytes = self.store.read_bytes(&output.digest)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            std::fs::write(&path, bytes)
                .with_context(|| format!("cannot restore output {}", output.path))?;
            #[cfg(unix)]
            if output.executable {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&path)?.permissions();
                permissions.set_mode(permissions.mode() | 0o111);
                std::fs::set_permissions(&path, permissions)
                    .with_context(|| format!("cannot make {} executable", output.path))?;
            }
            notes.push(format!(
                "output {} was missing and was restored from its receipt",
                output.path
            ));
        }
        Ok(notes)
    }
}
