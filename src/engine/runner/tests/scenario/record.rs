//! What runs of the scenario leave in the store, one canonical line each,
//! and the record of what two runs of it must leave.

use serde::Serialize;

use crate::core::flow::Flow;
use crate::core::receipt::{RunManifest, FLOT_VERSION};
use crate::engine::keying::{self, Facts};
use crate::engine::runner::RunReport;
use crate::store::Store;

/// What `runs` left in `store`, one canonical JSON value per line.
///
/// Stripped, because they differ from one run to the next: a manifest's
/// `run_id`, `started_at_ms` and `duration_ms`, a report's `duration_ms`,
/// and an observation's `started_at_ms`, `duration_ms` and `host_os`.
/// Also a cache entry's `seal`, which follows from flot's version;
/// `Store::recall` checks it instead.
///
/// Named, so that a release leaves this as it is: flot's version, and the
/// digests that follow from it or from the stripped fields. Each is
/// checked before it is named: a receipt id is the digest of its
/// identity, a step key the key of the inputs and dependencies its
/// receipt records, a run root the root of its manifest's steps, and an
/// observation digest is that of a stored receipt's observation. The
/// run id a limitation quotes is named too.
pub(super) fn scenario_record(flow: &Flow, store: &Store, runs: &[RunReport]) -> String {
    use crate::core::digest;

    fn canonical<T: Serialize>(value: &T, strip: &[&str]) -> String {
        let text = digest::canonical_json(value)
            .replace(
                &format!("\"version\":\"{FLOT_VERSION}\""),
                "\"version\":\"@VERSION@\"",
            )
            .replace(
                &format!("\"flot_version\":\"{FLOT_VERSION}\""),
                "\"flot_version\":\"@VERSION@\"",
            );
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .retain(|key, _| !strip.contains(&key.as_str()));
        digest::canonical_bytes(&value)
    }

    // Receipts by the run and step that first name them.
    let mut labels: Vec<(String, String)> = Vec::new();
    let mut name = |receipt_id: &str, label: String, run: usize| {
        if labels.iter().any(|(id, _)| id == receipt_id) {
            return;
        }
        let label = if labels.iter().any(|(_, taken)| *taken == label) {
            format!("{label}.{run}")
        } else {
            label
        };
        labels.push((receipt_id.to_string(), label));
    };
    for (index, run) in runs.iter().enumerate() {
        let number = index + 1;
        for step in &run.manifest.steps {
            for (attempt, earlier) in step.attempts.iter().enumerate() {
                name(
                    &earlier.receipt_id,
                    format!("{}#{}", step.step_id, attempt + 1),
                    number,
                );
            }
            let kept = store.load_receipt(&step.receipt_id).unwrap();
            let label = match &kept.identity.candidate {
                Some(candidate) => format!("{}/{candidate}", step.step_id),
                None => step.step_id.clone(),
            };
            name(&step.receipt_id, label, number);
            for candidate in &step.candidates {
                for (attempt, earlier) in candidate.attempts.iter().enumerate() {
                    name(
                        &earlier.receipt_id,
                        format!("{}/{}#{}", step.step_id, candidate.name, attempt + 1),
                        number,
                    );
                }
                name(
                    &candidate.receipt_id,
                    format!("{}/{}", step.step_id, candidate.name),
                    number,
                );
            }
        }
    }

    let mut stored: Vec<String> = std::fs::read_dir(store.root().join("receipts"))
        .unwrap()
        .map(|entry| {
            let name = entry.unwrap().file_name().into_string().unwrap();
            format!("sha256:{}", name.strip_suffix(".json").unwrap())
        })
        .collect();
    stored.sort();
    let mut named: Vec<String> = labels.iter().map(|(id, _)| id.clone()).collect();
    named.sort();
    assert_eq!(stored, named, "every stored receipt is one a run names");

    let mut names: Vec<(String, String)> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for (receipt_id, label) in &labels {
        let receipt = store.load_receipt(receipt_id).unwrap();
        assert!(receipt.is_authentic(), "{label}");
        let identity = &receipt.identity;
        let step = flow.step(&identity.step_id).unwrap();
        let step = match &identity.candidate {
            Some(candidate) => step.as_candidate(candidate),
            None => step.clone(),
        };
        let facts = Facts {
            inputs: identity.inputs.clone(),
            verify_inputs: identity.verify_inputs.clone(),
            limitations: Vec::new(),
        };
        assert_eq!(
            identity.step_key,
            keying::step_key(&flow.meta.name, &step, &facts, &identity.deps),
            "{label}"
        );
        names.push((receipt_id.clone(), format!("@RECEIPT:{label}@")));
        names.push((receipt.observed_digest(), format!("@OBSERVED:{label}@")));
        if !names.iter().any(|(digest, _)| *digest == identity.step_key) {
            names.push((identity.step_key.clone(), format!("@KEY:{label}@")));
        }
        lines.push(format!(
            "receipt {label} identity {}",
            canonical(identity, &[])
        ));
        lines.push(format!(
            "receipt {label} observed {}",
            canonical(
                &receipt.observed,
                &["started_at_ms", "duration_ms", "host_os"]
            )
        ));
    }

    let mut entries: Vec<serde_json::Value> = std::fs::read_dir(store.root().join("cache"))
        .unwrap()
        .map(|entry| {
            serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap()).unwrap()
        })
        .collect();
    entries.sort_by_key(|entry| {
        let receipt_id = entry["receipt_id"].as_str().unwrap();
        labels.iter().position(|(id, _)| id == receipt_id)
    });
    for entry in &entries {
        let step_key = entry["step_key"].as_str().unwrap();
        let recalled = store.recall(step_key).unwrap().expect("a sealed entry");
        assert_eq!(
            recalled.receipt.receipt_id,
            entry["receipt_id"].as_str().unwrap()
        );
        lines.push(format!("cache {}", canonical(entry, &["seal"])));
    }

    let mut sections: Vec<String> = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let number = index + 1;
        let manifest = &run.manifest;
        let pairs: Vec<(String, String)> = manifest
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), step.receipt_id.clone()))
            .collect();
        assert_eq!(manifest.root_digest, RunManifest::root_of(&pairs));
        assert_eq!(
            manifest.run_id,
            RunManifest::id_for(manifest.started_at_ms, &manifest.root_digest)
        );
        names.push((manifest.root_digest.clone(), format!("@ROOT:{number}@")));
        names.push((manifest.run_id.clone(), format!("@RUN:{number}@")));
        sections.push(format!(
            "run {number} manifest {}",
            canonical(manifest, &["run_id", "started_at_ms", "duration_ms"])
        ));
        for report in &run.steps {
            sections.push(format!(
                "run {number} report {} {}",
                report.step_id,
                canonical(report, &["duration_ms"])
            ));
        }
    }
    sections.extend(lines);

    let mut record = sections.join("\n");
    record.push('\n');
    for (digest, name) in names {
        record = record.replace(&digest, &name);
    }
    record
}

pub(super) const SCENARIO_RECORD: &str = r##"run 1 manifest {"claims":{"refuted":0,"unverified":2,"verified":3},"flot_version":"@VERSION@","flow":"scenario","flow_digest":"sha256:8ca2f9f300d6270baaede4b2f53d834abebfff569ee438772456c68392bce0d4","limitations":["build: ignored unknown metrics key `retries`; known keys are input_tokens, output_tokens, cost_usd","flaky: took 2 attempts of 2 allowed: 1 failed (exit 1), 2 held","lint: claims are unverified because the step did not succeed","pick: candidates slow held ($0.020000), fast held ($0.010000); kept fast","tolerant: input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","tolerant: exited 1 but the step is declared allow_failure, so the run is not failed by it","tolerant: produced no file for declared output report.txt, but the step is declared allow_failure","tolerant: changed files outside may_change (stray.txt), but the step is declared allow_failure","ship: not run: dependency lint (failed) did not hold","proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)","4 executed step(s) reported no metrics (flaky, flaky, lint, tolerant); the cost ceiling cannot see what they spent"],"max_cost_micro_usd":1000000,"max_parallel":1,"root_digest":"@ROOT:1@","spend":{"cost_micro_usd":280000,"input_tokens":7,"output_tokens":0,"reporting_steps":3,"reused_cost_micro_usd":0,"silent_steps":4,"unpriced_steps":0},"steps":[{"cached":false,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:build@","receipt_id":"@RECEIPT:build@","status":"ok","step_id":"build"},{"attempts":[{"observed_digest":"@OBSERVED:flaky#1@","receipt_id":"@RECEIPT:flaky#1@"}],"cached":false,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:flaky@","receipt_id":"@RECEIPT:flaky@","status":"ok","step_id":"flaky"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:lint@","receipt_id":"@RECEIPT:lint@","status":"failed","step_id":"lint"},{"cached":false,"candidates":[{"name":"slow","observed_digest":"@OBSERVED:pick/slow@","receipt_id":"@RECEIPT:pick/slow@"}],"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:pick/fast@","receipt_id":"@RECEIPT:pick/fast@","status":"ok","step_id":"pick"},{"cached":false,"claims":{"refuted":0,"unverified":0,"verified":0},"observed_digest":"@OBSERVED:tolerant@","receipt_id":"@RECEIPT:tolerant@","status":"ok","step_id":"tolerant"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:ship@","receipt_id":"@RECEIPT:ship@","status":"cancelled","step_id":"ship"}],"verdict":"failed"}
run 1 report build {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":[],"metrics":{"cost_micro_usd":250000,"input_tokens":7,"output_tokens":0},"outputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"receipt_id":"@RECEIPT:build@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:efa19f8f1d5ded57870b2f1e3c2ce2e76bacfe57bd8b2461802e9b5c07b74e7e","stdout_inline":"building\n","stdout_truncated":false,"step_id":"build"}
run 1 report flaky {"attempts":2,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":["took 2 attempts of 2 allowed: 1 failed (exit 1), 2 held"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:flaky@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"flaky"}
run 1 report lint {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["claims are unverified because the step did not succeed"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:lint@","status":"failed","stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"lint"}
run 1 report pick {"attempts":1,"cached":false,"candidate":"fast","claims":{"refuted":0,"unverified":0,"verified":1},"limitations":["candidates slow held ($0.020000), fast held ($0.010000); kept fast"],"metrics":{"cost_micro_usd":10000,"input_tokens":0,"output_tokens":0},"outputs":[{"digest":"sha256:f10264dbe79d60da1b1973f97059e082f740991b0ee07c519a86eaa5fe94c6ea","path":"pick.txt"}],"receipt_id":"@RECEIPT:pick/fast@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"pick"}
run 1 report tolerant {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":0},"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure","changed files outside may_change (stray.txt), but the step is declared allow_failure"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:tolerant@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","stdout_inline":"trying\n","stdout_truncated":false,"step_id":"tolerant"}
run 1 report ship {"attempts":0,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["not run: dependency lint (failed) did not hold"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:ship@","status":"cancelled","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"ship"}
run 2 manifest {"claims":{"refuted":0,"unverified":2,"verified":3},"flot_version":"@VERSION@","flow":"scenario","flow_digest":"sha256:8ca2f9f300d6270baaede4b2f53d834abebfff569ee438772456c68392bce0d4","limitations":["build: output out.txt was missing and was restored from its receipt","lint: claims are unverified because the step did not succeed","lint: its proof changed since run @RUN:1@ (lint_check.sh); read the change before trusting the verdict","tolerant: input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","tolerant: exited 1 but the step is declared allow_failure, so the run is not failed by it","tolerant: produced no file for declared output report.txt, but the step is declared allow_failure","ship: not run: dependency lint (failed) did not hold","proofs are held to no revision: an edited proof passes here; set proofs_from (flows.md#proofs-from-a-revision)","2 executed step(s) reported no metrics (lint, tolerant); the cost ceiling cannot see what they spent"],"max_cost_micro_usd":1000000,"max_parallel":1,"root_digest":"@ROOT:2@","spend":{"cost_micro_usd":0,"input_tokens":0,"output_tokens":0,"reporting_steps":0,"reused_cost_micro_usd":260000,"silent_steps":2,"unpriced_steps":0},"steps":[{"cached":true,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:build@","receipt_id":"@RECEIPT:build@","status":"ok","step_id":"build"},{"cached":true,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:flaky@","receipt_id":"@RECEIPT:flaky@","status":"ok","step_id":"flaky"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:lint.2@","receipt_id":"@RECEIPT:lint.2@","status":"failed","step_id":"lint"},{"cached":true,"claims":{"refuted":0,"unverified":0,"verified":1},"observed_digest":"@OBSERVED:pick/fast@","receipt_id":"@RECEIPT:pick/fast@","status":"ok","step_id":"pick"},{"cached":false,"claims":{"refuted":0,"unverified":0,"verified":0},"observed_digest":"@OBSERVED:tolerant.2@","receipt_id":"@RECEIPT:tolerant.2@","status":"ok","step_id":"tolerant"},{"cached":false,"claims":{"refuted":0,"unverified":1,"verified":0},"observed_digest":"@OBSERVED:ship.2@","receipt_id":"@RECEIPT:ship.2@","status":"cancelled","step_id":"ship"}],"verdict":"failed"}
run 2 report build {"attempts":0,"cached":true,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":["output out.txt was missing and was restored from its receipt"],"metrics":{"cost_micro_usd":250000,"input_tokens":7,"output_tokens":0},"outputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"receipt_id":"@RECEIPT:build@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:efa19f8f1d5ded57870b2f1e3c2ce2e76bacfe57bd8b2461802e9b5c07b74e7e","stdout_inline":"building\n","stdout_truncated":false,"step_id":"build"}
run 2 report flaky {"attempts":0,"cached":true,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":1},"limitations":[],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:flaky@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"flaky"}
run 2 report lint {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["claims are unverified because the step did not succeed","its proof changed since run @RUN:1@ (lint_check.sh); read the change before trusting the verdict"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:lint.2@","status":"failed","stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"lint"}
run 2 report pick {"attempts":0,"cached":true,"candidate":"fast","claims":{"refuted":0,"unverified":0,"verified":1},"limitations":[],"metrics":{"cost_micro_usd":10000,"input_tokens":0,"output_tokens":0},"outputs":[{"digest":"sha256:f10264dbe79d60da1b1973f97059e082f740991b0ee07c519a86eaa5fe94c6ea","path":"pick.txt"}],"receipt_id":"@RECEIPT:pick/fast@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"pick"}
run 2 report tolerant {"attempts":1,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":0,"verified":0},"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:tolerant.2@","status":"ok","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","stdout_inline":"trying\n","stdout_truncated":false,"step_id":"tolerant"}
run 2 report ship {"attempts":0,"cached":false,"candidate":null,"claims":{"refuted":0,"unverified":1,"verified":0},"limitations":["not run: dependency lint (failed) did not hold"],"metrics":null,"outputs":[],"receipt_id":"@RECEIPT:ship.2@","status":"cancelled","stderr_inline":"","stderr_truncated":false,"stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_inline":"","stdout_truncated":false,"step_id":"ship"}
receipt build identity {"claims":[{"reason":null,"status":"verified","text":"out.txt says built","verify_command":"sh check.sh","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"printf 'built\\n' > out.txt; printf '{\"cost_usd\": 0.25, \"input_tokens\": 7, \"retries\": 1}' > \"$FLOT_METRICS\"; echo building","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:efa19f8f1d5ded57870b2f1e3c2ce2e76bacfe57bd8b2461802e9b5c07b74e7e","step_id":"build","step_key":"@KEY:build@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[{"digest":"sha256:c481f0032f2e89c470a45c9ae93832961e64b30676c875f3e8c95aa9d6d4e561","path":"check.sh"}]}
receipt build observed {"metrics":{"cost_micro_usd":250000,"input_tokens":7,"output_tokens":0},"metrics_notes":["ignored unknown metrics key `retries`; known keys are input_tokens, output_tokens, cost_usd"],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":9,"stdout_inline":"building\n","stdout_truncated":false}
receipt flaky#1 identity {"claims":[{"reason":"step did not succeed","status":"unverified","text":"it held","verify_command":"true","verify_exit":null,"verify_output_digest":null}],"command":"test \"$FLOT_ATTEMPT\" = 2","deps":[],"exit_code":1,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":["claims are unverified because the step did not succeed"],"outputs":[],"status":"failed","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"flaky","step_key":"@KEY:flaky#1@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt flaky#1 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt flaky identity {"claims":[{"reason":null,"status":"verified","text":"it held","verify_command":"true","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"test \"$FLOT_ATTEMPT\" = 2","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"flaky","step_key":"@KEY:flaky#1@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt flaky observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt lint identity {"claims":[{"reason":"step did not succeed","status":"unverified","text":"src.txt is clean","verify_command":"sh lint_check.sh","verify_exit":null,"verify_output_digest":null}],"command":"echo 'src.txt: 2 problems' >&2; exit 3","deps":[],"exit_code":3,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":["claims are unverified because the step did not succeed"],"outputs":[],"status":"failed","stderr_digest":"sha256:02745cd98faf87c5c3d6ca04b0427e3256769d9feb9b1d8e1ed330afd31c0916","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"lint","step_key":"@KEY:lint@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[{"digest":"sha256:28d3b9e880a77975493dc7e359144c0295a4f694cfe0af4f928c22307bc5c320","path":"lint_check.sh"}]}
receipt lint observed {"metrics":null,"metrics_notes":[],"stderr_bytes":20,"stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt pick/fast identity {"candidate":"fast","claims":[{"reason":null,"status":"verified","text":"pick.txt names a candidate","verify_command":"test -s pick.txt","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"printf '%s\\n' \"$FLOT_CANDIDATE\" > pick.txt; if [ \"$FLOT_CANDIDATE\" = fast ]; then cost=0.01; else cost=0.02; fi; printf '{\"cost_usd\": %s}' \"$cost\" > \"$FLOT_METRICS\"","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[{"digest":"sha256:f10264dbe79d60da1b1973f97059e082f740991b0ee07c519a86eaa5fe94c6ea","path":"pick.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"pick","step_key":"@KEY:pick/fast@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt pick/fast observed {"metrics":{"cost_micro_usd":10000,"input_tokens":0,"output_tokens":0},"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt pick/slow identity {"candidate":"slow","claims":[{"reason":null,"status":"verified","text":"pick.txt names a candidate","verify_command":"test -s pick.txt","verify_exit":0,"verify_output_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}],"command":"printf '%s\\n' \"$FLOT_CANDIDATE\" > pick.txt; if [ \"$FLOT_CANDIDATE\" = fast ]; then cost=0.01; else cost=0.02; fi; printf '{\"cost_usd\": %s}' \"$cost\" > \"$FLOT_METRICS\"","deps":[],"exit_code":0,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":[],"outputs":[{"digest":"sha256:4c4a4a89dddfad0c9d436f7b5ebf11fc390938a70a40e4226b4bc8cd423ba83e","path":"pick.txt"}],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"pick","step_key":"@KEY:pick/slow@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt pick/slow observed {"metrics":{"cost_micro_usd":20000,"input_tokens":0,"output_tokens":0},"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt tolerant identity {"changed":{"stray.txt":"sha256:43bab6c26bc03299f3e5108f37cfa190ef6446cfe38f4229204a0d6b88e4b102","trace.log":"sha256:ea4f8e5663bf24ef621b5f3bfe0f251e1f9d63d25cfdab35a3ff33ac277d8cc6"},"claims":[],"command":"echo trying; printf 'trace\\n' > trace.log; printf 'stray\\n' > stray.txt; exit 1","deps":[],"exit_code":1,"flow":"scenario","inputs":[],"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure","changed files outside may_change (stray.txt), but the step is declared allow_failure"],"outputs":[],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","step_id":"tolerant","step_key":"@KEY:tolerant@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt tolerant observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":7,"stdout_inline":"trying\n","stdout_truncated":false}
receipt ship identity {"claims":[{"reason":"step was not run","status":"unverified","text":"shipped","verify_command":"true","verify_exit":null,"verify_output_digest":null}],"command":"cat out.txt","deps":[{"receipt_id":"@RECEIPT:build@","step_id":"build"},{"receipt_id":"@RECEIPT:lint@","step_id":"lint"}],"exit_code":null,"flow":"scenario","inputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"limitations":["not run: dependency lint (failed) did not hold"],"outputs":[],"status":"cancelled","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"ship","step_key":"@KEY:ship@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt ship observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt lint.2 identity {"claims":[{"reason":"step did not succeed","status":"unverified","text":"src.txt is clean","verify_command":"sh lint_check.sh","verify_exit":null,"verify_output_digest":null}],"command":"echo 'src.txt: 2 problems' >&2; exit 3","deps":[],"exit_code":3,"flow":"scenario","inputs":[{"digest":"sha256:536e506bb90914c243a12b397b9a998f85ae2cbd9ba02dfd03a9e155ca5ca0f4","path":"src.txt"}],"limitations":["claims are unverified because the step did not succeed"],"outputs":[],"status":"failed","stderr_digest":"sha256:02745cd98faf87c5c3d6ca04b0427e3256769d9feb9b1d8e1ed330afd31c0916","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"lint","step_key":"@KEY:lint.2@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[{"digest":"sha256:0c6868c2c44f053619cef1cc383e1d530743b574ca192ace9168a9ccf46a86e3","path":"lint_check.sh"}]}
receipt lint.2 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":20,"stderr_inline":"src.txt: 2 problems\n","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
receipt tolerant.2 identity {"changed":{"stray.txt":"sha256:43bab6c26bc03299f3e5108f37cfa190ef6446cfe38f4229204a0d6b88e4b102","trace.log":"sha256:ea4f8e5663bf24ef621b5f3bfe0f251e1f9d63d25cfdab35a3ff33ac277d8cc6"},"claims":[],"command":"echo trying; printf 'trace\\n' > trace.log; printf 'stray\\n' > stray.txt; exit 1","deps":[],"exit_code":1,"flow":"scenario","inputs":[],"limitations":["input pattern `notes/*.md` matched no readable file; this step's cache key cannot see those changes","exited 1 but the step is declared allow_failure, so the run is not failed by it","produced no file for declared output report.txt, but the step is declared allow_failure"],"outputs":[],"status":"ok","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:2790f7552a86f9bb2620979635d19be349ace8f1bb0a147e0dfb24f686e97707","step_id":"tolerant","step_key":"@KEY:tolerant@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt tolerant.2 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":7,"stdout_inline":"trying\n","stdout_truncated":false}
receipt ship.2 identity {"claims":[{"reason":"step was not run","status":"unverified","text":"shipped","verify_command":"true","verify_exit":null,"verify_output_digest":null}],"command":"cat out.txt","deps":[{"receipt_id":"@RECEIPT:build@","step_id":"build"},{"receipt_id":"@RECEIPT:lint.2@","step_id":"lint"}],"exit_code":null,"flow":"scenario","inputs":[{"digest":"sha256:56f6e6304d02d413bb7d5d463ac5cdc58551266dc7269b467fc385815f39b913","path":"out.txt"}],"limitations":["not run: dependency lint (failed) did not hold"],"outputs":[],"status":"cancelled","stderr_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","stdout_digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","step_id":"ship","step_key":"@KEY:ship.2@","tool":{"name":"flot","schema":5,"version":"@VERSION@"},"verify_inputs":[]}
receipt ship.2 observed {"metrics":null,"metrics_notes":[],"stderr_bytes":0,"stderr_inline":"","stderr_truncated":false,"stdout_bytes":0,"stdout_inline":"","stdout_truncated":false}
cache {"receipt_id":"@RECEIPT:build@","step_key":"@KEY:build@","vouched_by":[]}
cache {"receipt_id":"@RECEIPT:flaky@","step_key":"@KEY:flaky#1@","vouched_by":[]}
cache {"receipt_id":"@RECEIPT:pick/fast@","step_key":"@KEY:pick/fast@","vouched_by":[]}
cache {"receipt_id":"@RECEIPT:pick/slow@","step_key":"@KEY:pick/slow@","vouched_by":[]}
"##;
