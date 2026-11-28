//! The choices commands take that are not commands themselves: how a run
//! shows its steps, and what `flot key` and `flot trust` do.

use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

/// How `flot run` shows its steps as they run.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum Progress {
    Auto,
    Live,
    Plain,
    Off,
}

#[derive(Subcommand, Debug)]
pub(in super::super) enum KeyAction {
    /// Create the signing key if there is none, and print its public key.
    Init,
    /// Print the public key to share with people who import your bundles.
    Show,
    /// Replace the signing key, keeping the old secret beside it, and write a
    /// handover both keys sign for everyone who trusts the old one.
    Rotate {
        /// Where to write the handover [default: flot-handover-<key>.json].
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub(in super::super) enum TrustAction {
    /// Believe a signer: `flot trust add alice ed25519:...`.
    Add { name: String, key: String },
    /// Stop believing a signer, by name or key.
    Remove { name_or_key: String },
    /// Follow a trusted signer to their new key, given the handover their
    /// `flot key rotate` wrote.
    Rotate {
        handover: PathBuf,
        /// Stop trusting the old key as well, for a key that was exposed.
        #[arg(long)]
        revoke: bool,
    },
    /// List trusted signers.
    List,
}
