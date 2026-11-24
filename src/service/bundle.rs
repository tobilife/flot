//! Receipt bundles: one self-authenticating file that carries a run's evidence
//! to another host.
//!
//! A bundle is not a backup. It is the claim "this run happened, here is every
//! receipt and byte behind it", and import re-checks that claim before anything
//! enters the local store: receipts must hash to their own ids, objects must
//! hash to their digests, and every object a receipt references must be present.
//! A bundle that fails any of those is refused whole rather than partially
//! absorbed.
//!
//! Integrity is not authorship. A bundle may also carry attestations: Ed25519
//! signatures over the run manifest. An attestation that does not verify is a
//! sign of alteration and refuses the bundle; a valid one by an unknown key is
//! reported as `untrusted`, and the importer decides whether that is enough.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::core::attest::{self, Attestation, Authorship, SignerCheck};
use crate::core::digest;
use crate::core::fault::{self, FaultKind};
use crate::core::receipt::{Observed, Receipt, RunManifest};
use crate::store::Store;

/// 4: the manifest binds each receipt's observation (previews, spend).
pub const BUNDLE_SCHEMA: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub schema: u32,
    pub flot_version: String,
    pub run: RunManifest,
    pub receipts: Vec<Receipt>,
    /// digest -> base64 of the exact bytes.
    pub objects: BTreeMap<String, String>,
    /// Signatures over `run`. Empty for an unsigned bundle.
    pub attestations: Vec<Attestation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportReport {
    pub path: PathBuf,
    pub run_id: String,
    pub root_digest: String,
    pub receipts: usize,
    pub objects: usize,
    pub bytes: usize,
    /// Keys of everyone who signed the run the bundle carries.
    pub signed_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub run_id: String,
    pub root_digest: String,
    pub receipts: usize,
    pub objects: usize,
    /// Receipts and objects this store already held with identical content.
    pub already_present: usize,
    /// Whether local runs may reuse the imported receipts instead of executing.
    /// Only a trusted signer's receipts are; others are evidence to inspect.
    pub reusable: bool,
    pub authorship: Authorship,
    pub signers: Vec<SignerCheck>,
}

/// What an importer demands beyond integrity.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportPolicy {
    /// Refuse a bundle unless a key on the trust list validly signed it.
    pub require_trusted: bool,
}

/// Collect a run, everything it references, and every attestation recorded
/// for it. `attestation` adds a fresh signature, which is also recorded locally.
pub fn export(
    store: &Store,
    manifest: &RunManifest,
    attestation: Option<Attestation>,
) -> Result<Bundle> {
    if let Some(attestation) = &attestation {
        attest::check(attestation, manifest)
            .context("refusing to export with a signature that does not cover this run")?;
        store.add_attestations(&manifest.run_id, std::slice::from_ref(attestation))?;
    }
    let attestations = store.attestations(&manifest.run_id)?;
    for recorded in &attestations {
        if let Err(error) = attest::check(recorded, manifest) {
            bail!(
                "refusing to export run {}: its recorded attestation by {} no longer holds ({error})",
                manifest.run_id,
                attest::fingerprint(&recorded.key)
            );
        }
    }

    let mut receipts = Vec::new();
    let mut objects: BTreeMap<String, String> = BTreeMap::new();

    // Every step's receipt, and every earlier attempt's: two attempts that
    // did exactly the same work share one receipt.
    for (step_id, receipt_id, observed_digest) in manifest.evidence() {
        objects.insert(
            observed_digest.to_string(),
            encode(
                &store
                    .read_bytes(observed_digest)
                    .with_context(|| format!("step `{step_id}` has no readable observation"))?,
            ),
        );
        if receipts
            .iter()
            .any(|receipt: &Receipt| receipt.receipt_id == receipt_id)
        {
            continue;
        }
        let mut receipt = store
            .load_receipt(receipt_id)
            .with_context(|| format!("step `{step_id}` has no readable receipt"))?;
        if !receipt.is_authentic() {
            bail!("refusing to export step `{step_id}`: its receipt does not match its id");
        }
        for object in receipt.objects() {
            if objects.contains_key(object) {
                continue;
            }
            let bytes = store
                .read_bytes(object)
                .with_context(|| format!("step `{step_id}` references missing object {object}"))?;
            objects.insert(object.to_string(), encode(&bytes));
        }
        // The stored receipt carries its latest execution's observation; the
        // bundle carries this run's, which is what the manifest names.
        receipt.observed = store
            .load_observation(observed_digest)
            .with_context(|| format!("step `{step_id}` has no readable observation"))?;
        receipts.push(receipt);
    }

    Ok(Bundle {
        schema: BUNDLE_SCHEMA,
        flot_version: crate::core::receipt::FLOT_VERSION.to_string(),
        run: manifest.clone(),
        receipts,
        objects,
        attestations,
    })
}

/// Write a bundle as JSON and report what it carries.
pub fn write(bundle: &Bundle, path: &Path) -> Result<ExportReport> {
    let text = serde_json::to_string_pretty(bundle)?;
    std::fs::write(path, &text).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(ExportReport {
        path: path.to_path_buf(),
        run_id: bundle.run.run_id.clone(),
        root_digest: bundle.run.root_digest.clone(),
        receipts: bundle.receipts.len(),
        objects: bundle.objects.len(),
        bytes: text.len(),
        signed_by: bundle
            .attestations
            .iter()
            .map(|attestation| attestation.key.clone())
            .collect(),
    })
}

/// A bundle whose every piece checked out against every other.
struct Authenticated<'a> {
    decoded: BTreeMap<String, Vec<u8>>,
    observations: BTreeMap<&'a str, Observed>,
    receipts: BTreeMap<&'a str, &'a Receipt>,
    signers: Vec<SignerCheck>,
    authorship: Authorship,
}

fn authenticate<'a>(store: &Store, bundle: &'a Bundle) -> Result<Authenticated<'a>> {
    let mut decoded: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (object, encoded) in &bundle.objects {
        let bytes =
            decode(encoded).with_context(|| format!("object {object} is not valid base64"))?;
        let actual = digest::bytes(&bytes);
        if &actual != object {
            bail!("object {object} does not match its content (found {actual})");
        }
        decoded.insert(object.clone(), bytes);
    }

    let evidence = bundle.run.evidence();
    let named_receipts: std::collections::BTreeSet<&str> = evidence
        .iter()
        .map(|&(_, receipt_id, _)| receipt_id)
        .collect();
    if bundle.receipts.len() != named_receipts.len() {
        bail!(
            "bundle carries {} receipts for a run that names {}",
            bundle.receipts.len(),
            named_receipts.len()
        );
    }

    let stream = |object: &str| -> Result<Vec<u8>> {
        match decoded.get(object) {
            Some(bytes) => Ok(bytes.clone()),
            None => store.read_bytes(object),
        }
    };
    let mut observations: BTreeMap<&str, Observed> = BTreeMap::new();
    for &(step_id, _, observed_digest) in &evidence {
        let bytes = stream(observed_digest).with_context(|| {
            format!(
                "step `{step_id}` names observation {observed_digest}, which the bundle does not carry"
            )
        })?;
        let observed: Observed = serde_json::from_slice(&bytes).with_context(|| {
            format!("step `{step_id}` names an object that is not an observation")
        })?;
        observations.insert(observed_digest, observed);
    }

    let mut receipts: BTreeMap<&str, &Receipt> = BTreeMap::new();
    for receipt in &bundle.receipts {
        if !receipt.is_authentic() {
            bail!(
                "receipt for step `{}` does not match its id {}",
                receipt.identity.step_id,
                receipt.receipt_id
            );
        }
        if receipts
            .insert(receipt.receipt_id.as_str(), receipt)
            .is_some()
        {
            bail!("bundle carries receipt {} twice", receipt.receipt_id);
        }
        for object in receipt.objects() {
            if !decoded.contains_key(object) && !store.has_object(object) {
                bail!(
                    "step `{}` references object {object}, which the bundle does not carry",
                    receipt.identity.step_id
                );
            }
        }
    }

    // With equal counts, distinct receipts and every step finding its own,
    // the bundle carries exactly the run's receipts and nothing else.
    if let Some(problem) = bundle
        .run
        .disagreements(&receipts, &observations)
        .into_iter()
        .next()
    {
        bail!("bundled run does not follow from its receipts: {problem}");
    }
    // Previews are what a reader sees first; each must come from the output
    // its receipt names.
    for &(step_id, receipt_id, observed_digest) in &evidence {
        let receipt = receipts[receipt_id];
        if !observations[observed_digest].describes(
            &stream(&receipt.identity.stdout_digest)?,
            &stream(&receipt.identity.stderr_digest)?,
        ) {
            bail!("step `{step_id}` shows an output preview that is not its stored output");
        }
    }

    let (signers, authorship) =
        attest::assess(&bundle.attestations, &bundle.run, &store.trusted_keys()?);
    if let Some(broken) = signers.iter().find(|signer| !signer.is_valid()) {
        bail!(
            "attestation by {} does not hold: {}; the bundle was altered after signing",
            attest::fingerprint(&broken.key),
            broken.problem.as_deref().unwrap_or("invalid")
        );
    }
    // A bundle carries the run's evidence and nothing else: an object no
    // receipt or observation names would be stored, and never be audited.
    let named: std::collections::BTreeSet<&str> = bundle
        .receipts
        .iter()
        .flat_map(|receipt| receipt.objects())
        .chain(
            evidence
                .iter()
                .map(|&(_, _, observed_digest)| observed_digest),
        )
        .collect();
    if let Some(stray) = decoded
        .keys()
        .find(|object| !named.contains(object.as_str()))
    {
        bail!("bundle carries object {stray}, which none of its receipts names");
    }

    Ok(Authenticated {
        decoded,
        observations,
        receipts,
        signers,
        authorship,
    })
}

/// Authenticate a bundle and absorb it into the store.
///
/// Every check runs before the first write, so a rejected bundle leaves no
/// partial evidence behind.
pub fn import(store: &Store, bundle: &Bundle, policy: ImportPolicy) -> Result<ImportReport> {
    if bundle.schema != BUNDLE_SCHEMA {
        bail!(
            "bundle schema {} is not supported by this flot (expects {BUNDLE_SCHEMA})",
            bundle.schema
        );
    }

    // Everything that fails here is evidence that does not authenticate.
    let Authenticated {
        decoded,
        observations,
        receipts,
        signers,
        authorship,
    } = authenticate(store, bundle).map_err(|error| match super::fault_kind(&error) {
        FaultKind::Invalid => fault::integrity(format!("{error:#}")),
        _ => error,
    })?;

    if policy.require_trusted && authorship != Authorship::Trusted {
        bail!(
            "bundle is {} and a trusted signer is required; add the signer with `flot trust add` if you believe it",
            authorship.label()
        );
    }

    // A run id names one run. A different manifest under the same id is
    // refused rather than allowed to replace the one this store holds.
    let run_present = store.list_runs()?.contains(&bundle.run.run_id);
    if run_present && store.resolve_run(&bundle.run.run_id)? != bundle.run {
        return Err(fault::integrity(format!(
            "this store already holds a different run {}; the bundle would replace it",
            bundle.run.run_id
        )));
    }

    let _lock = store.lock(false)?;
    let mut already_present = 0usize;
    for (object, bytes) in &decoded {
        if store.has_object(object) {
            already_present += 1;
        }
        store.put_bytes(bytes)?;
    }
    // Seeding the cache lets a later local run reuse these receipts, and
    // reuse writes their output files into the workspace, where the next step
    // may execute them. Anyone can seal an authentic receipt claiming any
    // outcome, so only a signer this store trusts earns that, and only for as
    // long as it stays trusted: the entry records who vouched for it.
    let reusable = authorship == Authorship::Trusted;
    let vouched_by: Vec<String> = signers
        .iter()
        .filter(|signer| signer.trusted_as.is_some())
        .map(|signer| signer.key.clone())
        .collect();
    let mut stored = std::collections::BTreeSet::new();
    for (_, receipt_id, observed_digest) in bundle.run.evidence() {
        if !stored.insert(receipt_id) {
            continue;
        }
        // A receipt already here keeps its own observation; the run's is in
        // the observation object the manifest names.
        if store.load_receipt(receipt_id).is_ok() {
            already_present += 1;
        } else {
            let mut kept = receipts[receipt_id].clone();
            kept.observed = observations[observed_digest].clone();
            store.put_receipt(&kept)?;
        }
    }
    for step in &bundle.run.steps {
        let receipt = receipts[step.receipt_id.as_str()];
        // Only what local work would remember, and never in place of an
        // entry this store already relies on.
        if reusable && receipt.is_reusable() && store.recall(&receipt.identity.step_key)?.is_none()
        {
            store.remember(&receipt.identity.step_key, &receipt.receipt_id, &vouched_by)?;
        }
    }
    if !run_present {
        store.save_imported_run(&bundle.run)?;
    }
    if !bundle.attestations.is_empty() {
        store.add_attestations(&bundle.run.run_id, &bundle.attestations)?;
    }

    Ok(ImportReport {
        run_id: bundle.run.run_id.clone(),
        root_digest: bundle.run.root_digest.clone(),
        receipts: bundle.receipts.len(),
        objects: decoded.len(),
        already_present,
        reusable,
        authorship,
        signers,
    })
}

/// Largest bundle file `read` accepts. A bundle is parsed whole and its
/// objects decoded in memory, so an unbounded one could exhaust it.
pub const MAX_BUNDLE_BYTES: u64 = 1 << 30;

pub fn read(path: &Path) -> Result<Bundle> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("cannot read bundle {}", path.display()))?
        .len();
    if size > MAX_BUNDLE_BYTES {
        bail!(
            "bundle {} is {size} bytes; flot reads bundles up to {MAX_BUNDLE_BYTES} bytes",
            path.display()
        );
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read bundle {}", path.display()))?;
    parse(&text).with_context(|| format!("{} is not a flot bundle", path.display()))
}

/// Reads the schema before the body, so a bundle from another flot version is
/// named as such instead of failing on whichever field changed shape.
pub fn parse(text: &str) -> Result<Bundle> {
    #[derive(Deserialize)]
    struct Header {
        schema: u32,
    }
    let header: Header = serde_json::from_str(text)?;
    if header.schema != BUNDLE_SCHEMA {
        bail!(
            "bundle schema {} is not supported by this flot (expects {BUNDLE_SCHEMA})",
            header.schema
        );
    }
    Ok(serde_json::from_str(text)?)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Bundles must stay readable by any JSON tool,
/// and step output is arbitrary bytes, so text is not an option.
fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let bits = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(bits >> 18) as usize & 63] as char);
        out.push(ALPHABET[(bits >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(bits >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[bits as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn decode(text: &str) -> Result<Vec<u8>> {
    let value = |symbol: u8| -> Result<u32> {
        ALPHABET
            .iter()
            .position(|candidate| *candidate == symbol)
            .map(|position| position as u32)
            .with_context(|| format!("`{}` is not a base64 symbol", symbol as char))
    };
    let symbols: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !symbols.len().is_multiple_of(4) {
        bail!("base64 length {} is not a multiple of four", symbols.len());
    }
    let mut out = Vec::with_capacity(symbols.len() / 4 * 3);
    for quad in symbols.chunks(4) {
        let padding = quad.iter().filter(|symbol| **symbol == b'=').count();
        let bits = (value(quad[0])? << 18)
            | (value(quad[1])? << 12)
            | (if padding > 1 { 0 } else { value(quad[2])? } << 6)
            | (if padding > 0 { 0 } else { value(quad[3])? });
        out.push((bits >> 16) as u8);
        if padding < 2 {
            out.push((bits >> 8) as u8);
        }
        if padding < 1 {
            out.push(bits as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::flow::Flow;
    use crate::engine::runner::{RunOptions, Runner};
    use crate::store::HostSecret;

    const FLOW: &str = r#"
[flow]
name = "bundled"

[[step]]
id = "work"
run = "echo flot | tee artifact.txt"
claims = ["artifact.txt mentions flot"]
verify = "grep -q flot artifact.txt"
"#;

    fn recorded() -> (tempfile::TempDir, Store, RunManifest) {
        recorded_from(FLOW)
    }

    fn recorded_from(text: &str) -> (tempfile::TempDir, Store, RunManifest) {
        let dir = tempfile::tempdir().unwrap();
        let flow = Flow::parse(text).unwrap();
        let store = Store::open(dir.path(), HostSecret::from_bytes([7; 32])).unwrap();
        let manifest = {
            let runner = Runner::new(&flow, dir.path(), &store);
            runner.run(&RunOptions::default()).unwrap().manifest
        };
        (dir, store, manifest)
    }

    fn empty_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), HostSecret::from_bytes([7; 32])).unwrap();
        (dir, store)
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        let cases: [&[u8]; 7] = [b"", b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"];
        for data in cases {
            assert_eq!(decode(&encode(data)).unwrap(), data, "{data:?}");
        }
        let binary: Vec<u8> = (0u8..=255).collect();
        assert_eq!(decode(&encode(&binary)).unwrap(), binary);
    }

    #[test]
    fn base64_matches_known_encodings() {
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
    }

    #[test]
    fn a_bundle_carries_every_receipt_and_object_of_its_run() {
        let (_dir, store, manifest) = recorded();
        let bundle = export(&store, &manifest, None).unwrap();
        assert_eq!(bundle.receipts.len(), manifest.steps.len());
        for receipt in &bundle.receipts {
            for object in receipt.objects() {
                assert!(
                    bundle.objects.contains_key(object),
                    "a bundle must carry every object its receipts name"
                );
            }
        }
        // Three references and the run's observation, three objects: the empty
        // stderr and the empty proof output of `grep -q` share one digest.
        assert_eq!(bundle.objects.len(), 3);
    }

    #[test]
    fn an_imported_run_is_readable_and_auditable_on_a_fresh_store() {
        let (_source, store, manifest) = recorded();
        let bundle = export(&store, &manifest, None).unwrap();

        let (_target, fresh) = empty_store();
        let report = import(&fresh, &bundle, ImportPolicy::default()).unwrap();
        assert_eq!(report.receipts, manifest.steps.len());
        assert_eq!(report.already_present, 0);

        let imported = fresh.resolve_run(&manifest.run_id).unwrap();
        assert_eq!(imported.root_digest, manifest.root_digest);
        let audit = crate::service::audit::audit(&fresh, &imported).unwrap();
        assert!(audit.intact, "{:?}", audit.findings);
        assert_eq!(audit.verdict, manifest.verdict);
    }

    #[test]
    fn importing_twice_is_idempotent() {
        let (_source, store, manifest) = recorded();
        let bundle = export(&store, &manifest, None).unwrap();
        let (_target, fresh) = empty_store();
        import(&fresh, &bundle, ImportPolicy::default()).unwrap();
        let second = import(&fresh, &bundle, ImportPolicy::default()).unwrap();
        assert_eq!(second.already_present, second.receipts + second.objects);
    }

    #[test]
    fn a_tampered_object_is_refused_whole() {
        let (_source, store, manifest) = recorded();
        let mut bundle = export(&store, &manifest, None).unwrap();
        let victim = bundle.objects.keys().next().unwrap().clone();
        bundle.objects.insert(victim, encode(b"swapped"));

        let (_target, fresh) = empty_store();
        let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
        assert!(
            error.to_string().contains("does not match its content"),
            "{error}"
        );
        assert!(
            fresh.list_runs().unwrap().is_empty(),
            "a refused bundle must leave nothing behind"
        );
    }

    #[test]
    fn a_tampered_receipt_is_refused() {
        let (_source, store, manifest) = recorded();
        let mut bundle = export(&store, &manifest, None).unwrap();
        bundle.receipts[0].identity.command = "echo something-else".into();

        let (_target, fresh) = empty_store();
        let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
        assert!(
            error.to_string().contains("does not match its id"),
            "{error}"
        );
    }

    #[test]
    fn a_rewritten_run_root_is_refused() {
        let (_source, store, manifest) = recorded();
        let mut bundle = export(&store, &manifest, None).unwrap();
        bundle.run.root_digest = digest::bytes(b"forged root");

        let (_target, fresh) = empty_store();
        let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not follow from its receipts"),
            "{error}"
        );
    }

    #[test]
    fn a_bundle_missing_an_object_is_refused() {
        let (_source, store, manifest) = recorded();
        let mut bundle = export(&store, &manifest, None).unwrap();
        let victim = bundle.objects.keys().next().unwrap().clone();
        bundle.objects.remove(&victim);

        let (_target, fresh) = empty_store();
        let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("which the bundle does not carry"),
            "{error}"
        );
    }

    #[test]
    fn an_unsupported_schema_is_refused() {
        let (_source, store, manifest) = recorded();
        let mut bundle = export(&store, &manifest, None).unwrap();
        bundle.schema = BUNDLE_SCHEMA + 1;
        let (_target, fresh) = empty_store();
        assert!(import(&fresh, &bundle, ImportPolicy::default())
            .unwrap_err()
            .to_string()
            .contains("schema"));
    }

    #[test]
    fn a_bundle_from_another_schema_is_named_as_such_whatever_its_shape() {
        let (_source, store, manifest) = recorded();
        let bundle = export(&store, &manifest, None).unwrap();
        let mut value = serde_json::to_value(&bundle).unwrap();
        value["schema"] = serde_json::json!(BUNDLE_SCHEMA - 1);
        value["receipts"] = serde_json::json!([{"an": "older shape"}]);
        let error = format!("{:#}", parse(&value.to_string()).unwrap_err());
        assert!(
            error.contains(&format!(
                "bundle schema {} is not supported",
                BUNDLE_SCHEMA - 1
            )),
            "{error}"
        );
    }

    #[test]
    fn a_bundle_round_trips_through_a_file() {
        let (_source, store, manifest) = recorded();
        let bundle = export(&store, &manifest, None).unwrap();
        let path = _source.path().join("run.flotbundle.json");
        let report = write(&bundle, &path).unwrap();
        assert_eq!(report.receipts, bundle.receipts.len());
        assert!(report.bytes > 0);
        let reread = read(&path).unwrap();
        assert_eq!(reread.run.root_digest, manifest.root_digest);
    }

    const SIGNER: [u8; 32] = [3; 32];
    const STRANGER: [u8; 32] = [4; 32];

    fn trust(store: &Store, secret: &[u8; 32], name: &str) {
        let mut keys = store.trusted_keys().unwrap();
        keys.insert(attest::public_key_of(secret), name.to_string());
        store.save_trusted_keys(&keys).unwrap();
    }

    fn signed(store: &Store, manifest: &RunManifest, secret: &[u8; 32]) -> Bundle {
        export(store, manifest, Some(attest::sign(secret, manifest))).unwrap()
    }

    const REQUIRE_TRUSTED: ImportPolicy = ImportPolicy {
        require_trusted: true,
    };

    #[test]
    fn a_bundle_signed_by_a_trusted_key_imports_as_trusted() {
        let (_source, store, manifest) = recorded();
        let bundle = signed(&store, &manifest, &SIGNER);
        let (_target, fresh) = empty_store();
        trust(&fresh, &SIGNER, "alice");
        let report = import(&fresh, &bundle, REQUIRE_TRUSTED).unwrap();
        assert_eq!(report.authorship, Authorship::Trusted);
        assert_eq!(report.signers[0].trusted_as.as_deref(), Some("alice"));
        assert_eq!(
            fresh.attestations(&manifest.run_id).unwrap(),
            bundle.attestations
        );
    }

    /// Import `bundle` into a fresh project and run the same flow there.
    fn run_after_import(bundle: &Bundle, trusted: Option<&[u8; 32]>) -> (ImportReport, bool) {
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("flot.toml"), FLOW).unwrap();
        let fresh = Store::open(target.path(), HostSecret::from_bytes([7; 32])).unwrap();
        if let Some(secret) = trusted {
            trust(&fresh, secret, "alice");
        }
        let report = import(&fresh, bundle, ImportPolicy::default()).unwrap();
        let flow = Flow::parse(FLOW).unwrap();
        let run = Runner::new(&flow, target.path(), &fresh)
            .run(&RunOptions::default())
            .unwrap();
        (report, run.steps[0].cached)
    }

    #[test]
    fn a_trusted_signers_receipts_stand_in_for_local_work() {
        let (_source, store, manifest) = recorded();
        let (report, cached) = run_after_import(&signed(&store, &manifest, &SIGNER), Some(&SIGNER));
        assert!(report.reusable);
        assert!(cached, "trusted evidence from another host is reused");
    }

    #[test]
    fn revoking_a_signer_revokes_the_reuse_its_bundle_earned() {
        let (_source, store, manifest) = recorded();
        let bundle = signed(&store, &manifest, &SIGNER);
        let target = tempfile::tempdir().unwrap();
        let fresh = Store::open(target.path(), HostSecret::from_bytes([7; 32])).unwrap();
        trust(&fresh, &SIGNER, "alice");
        import(&fresh, &bundle, ImportPolicy::default()).unwrap();
        fresh.save_trusted_keys(&BTreeMap::new()).unwrap();

        let flow = Flow::parse(FLOW).unwrap();
        let run = Runner::new(&flow, target.path(), &fresh)
            .run(&RunOptions::default())
            .unwrap();
        assert!(
            !run.steps[0].cached,
            "an untrusted signer's receipt ran again"
        );
        assert!(run
            .manifest
            .limitations
            .iter()
            .any(|note| note.contains("no longer trusts")));
    }

    #[test]
    fn an_import_never_replaces_what_the_store_holds() {
        let (source, store, manifest) = recorded();
        // A local success the store relies on, for the same step key.
        let local = store
            .recall(
                &store
                    .load_receipt(&manifest.steps[0].receipt_id)
                    .unwrap()
                    .identity
                    .step_key,
            )
            .unwrap()
            .expect("the run was remembered");

        // A run under the same id that says something else.
        let mut other = export(&store, &manifest, None).unwrap();
        other.run.duration_ms += 1;
        let error = import(&store, &other, ImportPolicy::default()).unwrap_err();
        assert!(
            error.to_string().contains("already holds a different run"),
            "{error}"
        );

        // A trusted signer's receipt for the same work does not take over
        // the entry this host's own run wrote.
        let (_signer_dir, signer_store, signer_run) = recorded();
        trust(&store, &SIGNER, "alice");
        import(
            &store,
            &signed(&signer_store, &signer_run, &SIGNER),
            ImportPolicy::default(),
        )
        .unwrap();

        // The same run again is a no-op, and it stays this host's own run.
        import(
            &store,
            &export(&store, &manifest, None).unwrap(),
            ImportPolicy::default(),
        )
        .unwrap();
        assert!(!store.is_imported(&manifest.run_id));
        let still = store
            .recall(&local.receipt.identity.step_key)
            .unwrap()
            .unwrap();
        assert_eq!(still.receipt.receipt_id, local.receipt.receipt_id);
        assert!(still.vouched_by.is_empty());
        drop(source);
    }

    #[test]
    fn a_trusted_failure_is_evidence_not_cache() {
        let (_source, store, manifest) = recorded_from(
            "[flow]\nname = \"bundled\"\n\n[[step]]\nid = \"work\"\nrun = \"false\"\n",
        );
        let receipt = store.load_receipt(&manifest.steps[0].receipt_id).unwrap();
        assert!(!receipt.is_reusable());
        let (_target, fresh) = empty_store();
        trust(&fresh, &SIGNER, "alice");
        let report = import(
            &fresh,
            &signed(&store, &manifest, &SIGNER),
            ImportPolicy::default(),
        )
        .unwrap();
        assert_eq!(report.authorship, Authorship::Trusted);
        assert!(fresh.recall(&receipt.identity.step_key).unwrap().is_none());
        let entries = std::fs::read_dir(_target.path().join(".flot/cache"))
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(entries, 0, "nothing is remembered for a failure");
    }

    #[test]
    fn an_oversized_bundle_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.flotbundle.json");
        // Sparse: the size is real, the disk use is not.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_BUNDLE_BYTES + 1)
            .unwrap();
        let error = read(&path).unwrap_err().to_string();
        assert!(error.contains("flot reads bundles up to"), "{error}");
    }

    #[test]
    fn a_bundle_cannot_smuggle_objects_its_run_does_not_name() {
        let (_source, store, manifest) = recorded();
        let mut bundle = export(&store, &manifest, None).unwrap();
        bundle
            .objects
            .insert(digest::bytes(b"payload"), encode(b"payload"));
        let (_target, fresh) = empty_store();
        let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
        assert!(
            error.to_string().contains("none of its receipts names"),
            "{error}"
        );
        assert!(fresh.list_runs().unwrap().is_empty());
    }

    #[test]
    fn receipts_nobody_trusted_signed_are_evidence_not_cache() {
        let (_source, store, manifest) = recorded();
        // Anyone can seal an authentic receipt that claims any outcome, so
        // neither an unsigned nor a stranger-signed one may replace a run.
        for bundle in [
            export(&store, &manifest, None).unwrap(),
            signed(&store, &manifest, &STRANGER),
        ] {
            let (report, cached) = run_after_import(&bundle, Some(&SIGNER));
            assert!(!report.reusable);
            assert!(!cached, "{:?}", report.authorship);
        }
    }

    #[test]
    fn a_manifest_that_misstates_its_receipts_is_refused() {
        let (_source, store, manifest) = recorded();
        type Lie = fn(&mut RunManifest);
        let lies: [(&str, Lie); 5] = [
            ("verdict", |run| {
                run.verdict = crate::core::receipt::Verdict::Failed
            }),
            ("claim counts", |run| run.steps[0].claims.verified += 1),
            ("not derived", |run| run.run_id = "../../escape".into()),
            ("does not carry", |run| {
                run.steps[0].observed_digest = digest::bytes(b"other")
            }),
            ("appears more than once", |run| {
                let copy = run.steps[0].clone();
                run.steps.push(copy);
            }),
        ];
        for (expected, lie) in lies {
            let mut bundle = export(&store, &manifest, None).unwrap();
            lie(&mut bundle.run);
            let (_target, fresh) = empty_store();
            let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
            assert!(error.to_string().contains(expected), "{expected}: {error}");
            assert!(fresh.list_runs().unwrap().is_empty());
        }
    }

    /// Swap the run's observation of its first step for an edited copy, and
    /// re-derive everything an unsigned forger can: the object, its digest
    /// in the manifest, and the run id that root and start time imply.
    fn forge_observation(bundle: &mut Bundle, edit: impl Fn(&mut Observed)) {
        let named = bundle.run.steps[0].observed_digest.clone();
        let mut observed: Observed =
            serde_json::from_slice(&decode(&bundle.objects[&named]).unwrap()).unwrap();
        edit(&mut observed);
        let text = digest::canonical_json(&observed);
        let forged = digest::bytes(text.as_bytes());
        bundle
            .objects
            .insert(forged.clone(), encode(text.as_bytes()));
        bundle.run.steps[0].observed_digest = forged;
        bundle.receipts[0].observed = observed;
    }

    #[test]
    fn a_forged_preview_or_spend_is_refused() {
        let (_source, store, manifest) = recorded();
        let mut preview = export(&store, &manifest, None).unwrap();
        forge_observation(&mut preview, |observed| {
            observed.stdout_inline = "ALL TESTS PASSED\n".into()
        });
        let mut spend = export(&store, &manifest, None).unwrap();
        forge_observation(&mut spend, |observed| {
            observed.metrics = Some(crate::core::metrics::Metrics {
                input_tokens: 1,
                output_tokens: 1,
                cost_micro_usd: Some(1),
            })
        });
        for (bundle, expected) in [
            (preview, "not its stored output"),
            (spend, "run spend disagrees"),
        ] {
            let (_target, fresh) = empty_store();
            let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
            assert!(error.to_string().contains(expected), "{expected}: {error}");
        }
    }

    #[test]
    fn a_signed_observation_cannot_be_edited() {
        let (_source, store, manifest) = recorded();
        let mut bundle = signed(&store, &manifest, &SIGNER);
        forge_observation(&mut bundle, |observed| observed.duration_ms += 1);
        let (_target, fresh) = empty_store();
        trust(&fresh, &SIGNER, "alice");
        let error = import(&fresh, &bundle, REQUIRE_TRUSTED).unwrap_err();
        assert!(
            error.to_string().contains("altered after signing"),
            "{error}"
        );
    }

    #[test]
    fn a_receipts_own_observation_in_a_bundle_is_never_believed() {
        let (_source, store, manifest) = recorded();
        let mut bundle = export(&store, &manifest, None).unwrap();
        bundle.receipts[0].observed.stdout_inline = "ALL TESTS PASSED\n".into();
        let (_target, fresh) = empty_store();
        import(&fresh, &bundle, ImportPolicy::default()).unwrap();
        let kept = fresh.load_receipt(&manifest.steps[0].receipt_id).unwrap();
        assert_eq!(
            kept.observed,
            store
                .load_observation(&manifest.steps[0].observed_digest)
                .unwrap()
        );
    }

    #[test]
    fn a_valid_signature_by_an_unknown_key_is_untrusted_not_trusted() {
        let (_source, store, manifest) = recorded();
        let bundle = signed(&store, &manifest, &STRANGER);
        let (_target, fresh) = empty_store();
        trust(&fresh, &SIGNER, "alice");
        let report = import(&fresh, &bundle, ImportPolicy::default()).unwrap();
        assert_eq!(report.authorship, Authorship::Untrusted);
    }

    #[test]
    fn requiring_trust_refuses_unsigned_and_untrusted_bundles_whole() {
        let (_source, store, manifest) = recorded();
        let (_target, fresh) = empty_store();
        trust(&fresh, &SIGNER, "alice");

        let unsigned = export(&store, &manifest, None).unwrap();
        let error = import(&fresh, &unsigned, REQUIRE_TRUSTED).unwrap_err();
        assert!(error.to_string().contains("bundle is unsigned"), "{error}");

        let stranger = export(&store, &manifest, Some(attest::sign(&STRANGER, &manifest))).unwrap();
        let error = import(&fresh, &stranger, REQUIRE_TRUSTED).unwrap_err();
        assert!(error.to_string().contains("bundle is untrusted"), "{error}");
        assert!(fresh.list_runs().unwrap().is_empty());
    }

    #[test]
    fn a_run_altered_after_signing_is_refused_even_with_valid_receipts() {
        let (_source, store, manifest) = recorded();
        let mut bundle = signed(&store, &manifest, &SIGNER);
        // Every receipt and the root stay valid; only an observed field moves.
        bundle.run.duration_ms += 1;
        let (_target, fresh) = empty_store();
        trust(&fresh, &SIGNER, "alice");
        let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
        assert!(
            error.to_string().contains("altered after signing"),
            "{error}"
        );
        assert!(fresh.list_runs().unwrap().is_empty());
    }

    #[test]
    fn a_stranger_cannot_claim_a_trusted_identity_by_swapping_the_key() {
        let (_source, store, manifest) = recorded();
        let mut bundle = signed(&store, &manifest, &STRANGER);
        bundle.attestations[0].key = attest::public_key_of(&SIGNER);
        let (_target, fresh) = empty_store();
        trust(&fresh, &SIGNER, "alice");
        let error = import(&fresh, &bundle, ImportPolicy::default()).unwrap_err();
        assert!(
            error.to_string().contains("signature does not verify"),
            "{error}"
        );
    }

    #[test]
    fn re_exporting_an_imported_run_carries_every_signature_forward() {
        let (_source, store, manifest) = recorded();
        let first = signed(&store, &manifest, &SIGNER);
        let (_relay, relay) = empty_store();
        import(&relay, &first, ImportPolicy::default()).unwrap();
        let second = export(&relay, &manifest, Some(attest::sign(&STRANGER, &manifest))).unwrap();
        let keys: Vec<&str> = second.attestations.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&attest::public_key_of(&SIGNER).as_str()));
        assert!(keys.contains(&attest::public_key_of(&STRANGER).as_str()));
    }

    #[test]
    fn a_signature_for_another_run_is_refused_at_export() {
        let (_source, store, manifest) = recorded();
        let mut other = manifest.clone();
        other.root_digest = digest::bytes(b"elsewhere");
        let error = export(&store, &manifest, Some(attest::sign(&SIGNER, &other))).unwrap_err();
        assert!(
            format!("{error:#}").contains("does not cover this run"),
            "{error:#}"
        );
    }
}
