//! The tools the server lists: the schema of each, which a call's arguments
//! are checked against, and hints of what it may change.

use serde_json::{json, Map, Value};

/// Check tool arguments against the schema the catalog advertises, so what a
/// host was told is what is enforced: an object, no unknown keys, required
/// keys present, declared types and minimums.
pub(super) fn check_arguments(
    schema: &Value,
    arguments: &Value,
) -> std::result::Result<(), String> {
    let Some(arguments) = arguments.as_object() else {
        return Err("`arguments` must be an object".into());
    };
    let properties = schema["properties"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (key, value) in arguments {
        let Some(property) = properties.get(key) else {
            return Err(format!("unknown argument `{key}`"));
        };
        let minimum = property.get("minimum").and_then(Value::as_f64);
        let fits = match property["type"].as_str() {
            Some("string") => value.is_string(),
            Some("boolean") => value.is_boolean(),
            Some("integer") => value
                .as_u64()
                .is_some_and(|number| minimum.is_none_or(|minimum| number as f64 >= minimum)),
            Some("number") => value.as_f64().is_some_and(|number| {
                number.is_finite() && minimum.is_none_or(|minimum| number >= minimum)
            }),
            _ => true,
        };
        if !fits {
            let kind = property["type"].as_str().unwrap_or("value");
            return Err(match minimum {
                Some(minimum) => format!("`{key}` must be a {kind} of at least {minimum}"),
                None => format!("`{key}` must be a {kind}"),
            });
        }
    }
    for key in schema["required"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap_or_default();
        if !arguments.contains_key(key) {
            return Err(format!("`{key}` is required"));
        }
    }
    Ok(())
}

/// Whether a tool only reads the store, or runs the flow's commands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    Reads,
    Runs,
}

fn tool(
    name: &str,
    effect: Effect,
    description: &str,
    properties: Value,
    required: &[&str],
) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    schema.insert("properties".into(), properties);
    schema.insert("required".into(), json!(required));
    schema.insert("additionalProperties".into(), json!(false));
    // Hosts use these hints to decide what needs a person's approval. Running
    // a flow executes arbitrary, unsandboxed commands, so it claims nothing
    // reassuring; reading the store changes nothing and reaches nothing.
    let annotations = match effect {
        Effect::Reads => json!({
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        }),
        Effect::Runs => json!({
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": true,
        }),
    };
    json!({
        "name": name,
        "description": description,
        "inputSchema": Value::Object(schema),
        "annotations": annotations,
    })
}

pub(super) fn tool_catalog() -> Vec<Value> {
    vec![
        tool(
            "flot_plan",
            Effect::Reads,
            "Show what the flow would run, what it would reuse from receipts, and which claims have no proof command. Runs nothing. Reuse after an `always` step is predicted on that step coming out as it did last time; such a step lists those steps in `assumes`.",
            json!({
                "only": { "type": "string", "description": "Limit to one step and its dependencies." },
                "force": { "type": "boolean", "description": "Ignore cached receipts." }
            }),
            &[],
        ),
        tool(
            "flot_run",
            Effect::Runs,
            "Execute the flow and record a receipt per step. Act on `result`: `passed` only when every step of the flow succeeded and every claim was proven; `partial` when every selected step passed but `only` left steps out, so nothing is known of those (the manifest's `partial` gives `selected` and `of`; its `verdict` still reads `passed`, which is not a pass of the flow); `unverified` when work completed but a claim has no proof; `failed` when a step failed or a claim was refuted. A step with `cached: true` was not executed again because the same command already succeeded over byte-identical inputs and upstream receipts, and every file it created, edited or deleted is still as it left it (its outputs are put back); its reused receipt is as strong as a fresh run, so do not force a re-run to double-check it. A reused step whose manifest entry has `vouched_by` did not run on this host: its receipt came from an imported run, and `vouched` lists each such step's signers with `confirmed` (this store holds a run the key signed that kept the same receipt) and `trusted_as` (the name this host trusts a confirmed key under). A step's inline output keeps its first and last lines; flot_step_output returns all of it.",
            json!({
                "only": { "type": "string", "description": "Limit to one step and its dependencies." },
                "force": { "type": "boolean", "description": "Execute every selected step even when an identical receipt could be reused; for steps that are flaky or depend on something undeclared." },
                "jobs": { "type": "integer", "minimum": 1, "description": "Run up to this many independent steps at once." },
                "max_cost_usd": { "type": "number", "minimum": 0, "description": "Stop starting new steps once executed steps have reported this many dollars through $FLOT_METRICS. It can only lower the flow's own `max_cost_usd`: when the flow sets one, the lower of the two applies." }
            }),
            &[],
        ),
        tool(
            "flot_show",
            Effect::Reads,
            "Summarize a recorded run: steps, claim verdicts and limitations. Accepts `latest` (the newest run executed here), a run id, or a prefix of at least 4 characters of a run id or its root.",
            json!({ "run": { "type": "string", "description": "Run selector; defaults to latest." } }),
            &[],
        ),
        tool(
            "flot_explain",
            Effect::Reads,
            "Say why a recorded run did not pass, so you know what to fix before running it again. `result` is what `flot_verify` gives; `causes` are what keeps the run as a whole from passing (`broken`, `partial`, `only_proofs_differ`); `steps` lists each step that did not hold, or only `step` when named, with its `causes`. Each cause has a `kind` (`failed`, `timed_out`, `not_run`, `sandbox`, `proof_changed`, `no_proof`, `refuted`, `held`, `fails_before`, `mutants`, `unproven`, `altered`), a `detail`, and `next`: the flot command that shows more, or what to change. `only_proofs_differ` and `proof_changed` are for a person to review: no run proves a changed proof.",
            json!({
                "run": { "type": "string", "description": "Run selector; defaults to latest." },
                "step": { "type": "string", "description": "Only this step, whether it held or not." }
            }),
            &[],
        ),
        tool(
            "flot_verify",
            Effect::Reads,
            "Re-authenticate a recorded run from the store alone: receipt bodies against their ids, dependency references, every evidence object re-read against its digest, and the run root. Act on `result`, the worst that holds, which is also what `flot verify` exits with: `broken` (exit 1, and `isError`) when the evidence does not authenticate, as `findings` says, and `verdict` is then only what the altered record claims; `failed` (exit 1) when a step failed or a claim was refuted; `unverified` (exit 2) when a claim has no proof or the proofs differ from the revision in `anchor`; `partial` (exit 2) when every selected step passed but the run left steps of the flow out, as `partial` gives `selected` of `of`; `passed` (exit 0) only otherwise. `authorship` says who vouched for the run, judged against this host's trust list now: `unsigned`; `untrusted` (validly signed by no key this host trusts); `this_host` (signed by this host's own key, which is not on its trust list: a label, not trust: anything running here as this user, an agent included, can sign with it); `trusted` (validly signed by a key on the trust list, named in `signers`). `vouched` lists the steps the run reused from imported runs, each with its signers marked `confirmed` (this store holds a run the key signed that kept the same receipt) and `trusted_as`; a signer not confirmed is a name the run gives, not a signature.",
            json!({ "run": { "type": "string", "description": "Run selector; defaults to latest." } }),
            &[],
        ),
        tool(
            "flot_diff",
            Effect::Reads,
            "Compare two runs by receipt identity. Identical receipts mean the same work over the same inputs reached the same verdict. Each changed step lists in `reasons` why, read from its two receipts: the files it or its proof read, its dependencies, its command, its claims, or, with the same key, the same work coming out otherwise.",
            json!({
                "left": { "type": "string", "description": "Run selector; defaults to latest~1, the run before the latest." },
                "right": { "type": "string", "description": "Run selector; defaults to latest." }
            }),
            &[],
        ),
        tool(
            "flot_receipt",
            Effect::Reads,
            "Read one receipt as structured data. Accepts a full receipt id or an unambiguous hex prefix.",
            json!({ "receipt_id": { "type": "string", "description": "Receipt id or hex prefix." } }),
            &["receipt_id"],
        ),
        tool(
            "flot_read_object",
            Effect::Reads,
            "Read a stored object, such as a step's complete stdout, by digest. Use the digest from a receipt when its inline preview was trimmed.",
            json!({
                "digest": { "type": "string", "description": "sha256:<hex> digest from a receipt." },
                "max_bytes": { "type": "integer", "minimum": 1, "description": "Inline byte ceiling; the complete object stays retrievable." }
            }),
            &["digest"],
        ),
        tool(
            "flot_step_output",
            Effect::Reads,
            "Read the complete output one step of a run printed, such as the failing test log behind a trimmed preview, or with `proof` what its proof printed, such as why a claim was refuted. By default the receipt the run kept; `candidate` and `attempt` read a candidate it did not keep or an attempt that did not hold.",
            json!({
                "step": { "type": "string", "description": "Step id." },
                "candidate": { "type": "string", "description": "Another candidate the run executed; defaults to the kept one." },
                "attempt": { "type": "integer", "minimum": 1, "description": "Attempt number from 1; defaults to the last." },
                "run": { "type": "string", "description": "Run selector; defaults to latest." },
                "stderr": { "type": "boolean", "description": "Read stderr instead of stdout." },
                "proof": { "type": "boolean", "description": "Read what the step's proof printed (stdout and stderr together) instead." },
                "held": { "type": "boolean", "description": "Read what the step's proof printed with its held files as the revision has them instead." },
                "mutants": { "type": "boolean", "description": "Read what the step's mutation tester printed instead." },
                "max_bytes": { "type": "integer", "minimum": 1, "description": "Inline byte ceiling; the complete object stays retrievable." }
            }),
            &["step"],
        ),
        tool(
            "flot_runs",
            Effect::Reads,
            "List recorded runs, newest first, with their verdicts, root digests and whether they were imported; runs that cannot be read are named under `unreadable`.",
            json!({ "limit": { "type": "integer", "minimum": 1, "description": "Maximum runs to return." } }),
            &[],
        ),
        tool(
            "flot_search",
            Effect::Reads,
            "Find text in recorded runs, newest first: each executed step's complete stdout and stderr, its proofs' output, its claims and their reasons, and limitations, including attempts and candidates that were not kept. The match is literal and ignores case. Use it to find when an error first appeared or which run a message came from; read a whole output with flot_step_output.",
            json!({
                "query": { "type": "string", "description": "Text to find." },
                "runs": { "type": "integer", "minimum": 1, "description": "How many of the newest runs to search; defaults to 50." },
                "limit": { "type": "integer", "minimum": 1, "description": "Stop after this many hits; defaults to 50." }
            }),
            &["query"],
        ),
    ]
}
