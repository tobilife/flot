//! Signing a run with the host key, and moving runs between hosts by bundle
//! or git remote, with what looks like a credential kept from leaving.

use std::path::Path;

use anyhow::{bail, Result};
use serde::Serialize;

use crate::core::fault;
use crate::core::receipt::RunManifest;
use crate::service::bundle::{self, ExportReport, ImportPolicy, ImportReport};
use crate::service::{audit, keys, share, Workspace};
use crate::store::Store;

/// How a run leaves this host, by `bundle` or `push`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Sharing {
    /// Attest the run with the host key first.
    pub sign: bool,
    /// Share it even when it carries credential-shaped tokens.
    pub allow_secrets: bool,
}

/// The bundle of a run about to leave this host. A run that carries what
/// looks like a credential is refused before anything is signed or written:
/// receipts are content-addressed, so a token cannot be masked out of the
/// evidence, only kept out of the run.
fn shareable(store: &Store, manifest: &RunManifest, share: Sharing) -> Result<bundle::Bundle> {
    let key = if share.sign {
        let key = keys::load(&keys::default_path()?)?;
        signable(store, manifest)?;
        Some(key)
    } else {
        None
    };
    let unsigned = bundle::export(store, manifest, None)?;
    if !share.allow_secrets {
        let found = bundle::exposures(&unsigned)?;
        if !found.is_empty() {
            let list: Vec<String> = found
                .iter()
                .map(|e| format!("  - {}: {} ({})", e.place, e.kind, e.shown))
                .collect();
            bail!(
                "refusing to share run {}: it carries what looks like credentials, and nothing left this host:\n{}\nkeep them out of the step's output and run it again, or pass --allow-secrets if they are not credentials",
                manifest.run_id,
                list.join("\n")
            );
        }
    }
    let Some(key) = key else {
        return Ok(unsigned);
    };
    attest_recorded(store, manifest, &key)?;
    bundle::export(store, manifest, None)
}

#[derive(Debug, Clone, Serialize)]
pub struct SignReport {
    pub run_id: String,
    pub root_digest: String,
    pub signer: String,
    /// False when this key had already signed the run.
    pub added: bool,
}

impl Workspace {
    /// Attest a run executed here with the host signing key and record the
    /// attestation in the store, so `verify` reports it and every later
    /// bundle of the run carries it.
    pub fn sign(&self, run: &str) -> Result<SignReport> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        sign_recorded(&store, &manifest, &keys::load(&keys::default_path()?)?)
    }

    /// Write one run as a portable bundle. Defaults to
    /// `<root>/<run-id>.flotbundle.json`. `sign` attests the run first, as
    /// `sign` does.
    pub fn bundle(&self, run: &str, out: Option<&Path>, share: Sharing) -> Result<ExportReport> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let path = out.map(Path::to_path_buf).unwrap_or_else(|| {
            self.root
                .join(format!("{}.flotbundle.json", manifest.run_id))
        });
        bundle::write(&shareable(&store, &manifest, share)?, &path)
    }

    /// Put a run on a git remote under [`share::RUNS_REF`]. `sign` attests
    /// the run first, as `sign` does.
    pub fn push(&self, run: &str, remote: &str, share: Sharing) -> Result<share::PushReport> {
        let store = self.store()?;
        let manifest = store.resolve_run(run)?;
        let bundle = shareable(&store, &manifest, share)?;
        share::push(&self.root, remote, &manifest, &bundle)
    }

    /// Import the runs of this flow that a git remote holds and this store
    /// does not.
    pub fn pull(&self, remote: &str, policy: ImportPolicy) -> Result<share::PullReport> {
        let flow = self.flow()?;
        share::pull(&self.root, &self.store()?, remote, &flow.meta.name, policy)
    }

    pub fn import(&self, path: &Path, policy: ImportPolicy) -> Result<ImportReport> {
        let store = self.store()?;
        bundle::import(&store, &bundle::read(path)?, policy)
    }
}

/// A signature says "this host ran this, and this is what it recorded". It
/// cannot say that of a run that arrived in a bundle, nor of a run whose
/// evidence no longer authenticates: signing either would vouch for something
/// this host did not see.
fn sign_recorded(
    store: &Store,
    manifest: &RunManifest,
    key: &keys::SigningKeyFile,
) -> Result<SignReport> {
    signable(store, manifest)?;
    attest_recorded(store, manifest, key)
}

/// Whether this host may sign the run; see [`sign_recorded`].
fn signable(store: &Store, manifest: &RunManifest) -> Result<()> {
    if store.is_imported(&manifest.run_id) {
        bail!(
            "run {} was imported, not run here; it can be bundled again but not signed by this host",
            manifest.run_id
        );
    }
    let audit = audit::audit(store, manifest)?;
    if !audit.intact {
        return Err(fault::integrity(format!(
            "run {} does not authenticate, so this host will not sign it: {}",
            manifest.run_id,
            audit.findings.join("; ")
        )));
    }
    Ok(())
}

/// Record the host's signature over a run [`signable`] accepted.
fn attest_recorded(
    store: &Store,
    manifest: &RunManifest,
    key: &keys::SigningKeyFile,
) -> Result<SignReport> {
    let signer = key.public_key();
    let added = !store
        .attestations(&manifest.run_id)?
        .iter()
        .any(|existing| existing.key == signer);
    store.add_attestations(&manifest.run_id, &[key.sign(manifest)])?;
    Ok(SignReport {
        run_id: manifest.run_id.clone(),
        root_digest: manifest.root_digest.clone(),
        signer,
        added,
    })
}
