//! What sharing runs and managing keys print: bundles, push and pull,
//! imports, the signing key and the trust list.

use crate::core::digest;
use crate::service::bundle::{ExportReport, ImportReport};
use crate::service::share::{PullReport, PushReport};
use crate::service::{KeyInfo, KeyRotation, TrustEntry, TrustRotation};

use super::super::terminal_safe;
use super::audit::render_authorship;

pub(in super::super) fn render_bundle(report: &ExportReport) {
    say!(
        "wrote {} ({} receipts, {} objects, {} bytes)  root {}",
        report.path.display(),
        report.receipts,
        report.objects,
        report.bytes,
        digest::short(&report.root_digest)
    );
    if report.signed_by.is_empty() {
        say!("unsigned: importers can check integrity but not who produced it");
    }
    for key in &report.signed_by {
        say!("signed by {key}");
    }
}

pub(in super::super) fn render_push(report: &PushReport) {
    if report.unchanged {
        say!(
            "run {} is already on {} as {}",
            report.run_id,
            report.remote,
            report.path
        );
    } else {
        say!(
            "pushed run {} to {} {}  {}  commit {}",
            report.run_id,
            report.remote,
            crate::service::share::RUNS_REF,
            report.path,
            &report.commit[..report.commit.len().min(12)]
        );
        if report.signed_by.is_empty() {
            say!("unsigned: others can inspect it, but only a signed run can stand in for their work");
        }
    }
}

pub(in super::super) fn render_pull(report: &PullReport) {
    if report.empty {
        say!(
            "{} has no shared runs ({})",
            report.remote,
            crate::service::share::RUNS_REF
        );
    } else {
        for imported in &report.imported {
            say!(
                "imported run {}  root {}  {}{}",
                imported.run_id,
                digest::short(&imported.root_digest),
                imported.authorship.label(),
                if imported.reusable { "  reusable" } else { "" }
            );
        }
        for refused in &report.refused {
            say!("refused  {}: {}", refused.path, refused.reason);
        }
        say!(
            "{} of flow {}: {} imported, {} already here, {} refused",
            report.remote,
            report.flow,
            report.imported.len(),
            report.present.len(),
            report.refused.len()
        );
    }
}

pub(in super::super) fn render_import(report: &ImportReport) {
    say!(
        "imported run {}  root {}  {} receipts, {} objects; {} of them already present",
        report.run_id,
        digest::short(&report.root_digest),
        report.receipts,
        report.objects,
        report.already_present
    );
    render_authorship(report.authorship, &report.signers);
    if report.reusable {
        say!("reusable: local runs may reuse these receipts and their outputs");
    } else {
        say!("not reusable: only a trusted signer's receipts can stand in for local work");
    }
    say!("next: flot verify {}", report.run_id);
}

pub(in super::super) fn render_key_rotate(rotation: &KeyRotation) {
    say!("rotated signing key {}", rotation.path.display());
    say!("new  {}", rotation.new_key);
    say!("old  {}", rotation.old_key);
    say!("     kept as {}", rotation.retired.display());
    say!("handover {}", rotation.handover.display());
    say!("next: send it to everyone who trusts the old key; they run `flot trust rotate <file>`");
}

pub(in super::super) fn render_key(info: &KeyInfo) {
    let state = if info.created { "created" } else { "using" };
    say!("{state} signing key {}", info.path.display());
    say!("{}", info.public_key);
}

pub(in super::super) fn render_trust_add(entry: &TrustEntry) {
    say!("trusting {} as {}", entry.key, entry.name);
}

pub(in super::super) fn render_trust_remove(entry: &TrustEntry) {
    say!("no longer trusting {} ({})", entry.name, entry.key);
}

pub(in super::super) fn render_trust_rotate(rotation: &TrustRotation) {
    let state = if rotation.added {
        "trusting"
    } else {
        "already trusting"
    };
    say!("{state} {} as {}", rotation.to, rotation.name);
    if rotation.revoked {
        say!("no longer trusting {}", rotation.from);
    } else {
        say!(
            "still trusting {} as {}; `flot trust remove {}` once its runs no longer matter",
            rotation.from,
            rotation.name,
            rotation.from
        );
    }
}

pub(in super::super) fn render_trust_list(entries: &[TrustEntry]) {
    if entries.is_empty() {
        say!("no trusted signers");
    } else {
        for entry in entries {
            say!("{:<16} {}", entry.name, entry.key);
        }
    }
}
