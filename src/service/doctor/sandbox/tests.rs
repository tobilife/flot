//! Tests of what the sandbox check says, for each thing a host may lack and
//! each thing a step may ask for.

use crate::core::flow::Flow;

use super::super::Status;
use super::{sandbox_check, Support};

const LANDLOCK_2: &str = "cannot deny truncation on Landlock ABI 2; ABI 3 (Linux 6.2) is required";
const NO_USERNS: &str = "cannot start a confined child: Operation not permitted (os error 1)";

fn flow(sandboxes: &[&str]) -> Flow {
    let mut text = String::from("[flow]\nname = \"t\"\n");
    for (index, sandbox) in sandboxes.iter().enumerate() {
        text.push_str(&format!(
            "\n[[step]]\nid = \"s{index}\"\nrun = \"true\"\nsandbox = {sandbox}\n"
        ));
    }
    text.push_str("\n[[step]]\nid = \"free\"\nrun = \"true\"\n");
    Flow::parse(&text).unwrap()
}

fn support(network: Result<&str, &str>, write: Result<&str, &str>) -> Support {
    Support {
        network: network.map(str::to_string).map_err(str::to_string),
        write: write.map(str::to_string).map_err(str::to_string),
    }
}

fn said(flow: Option<&Flow>, support: &Support) -> (Status, String) {
    let check = sandbox_check(flow, support);
    assert_eq!(check.check, "sandbox");
    (check.status, check.detail)
}

#[test]
fn a_host_that_confines_both_ways_says_how_whether_or_not_a_step_asks() {
    let full = support(Ok("network namespace"), Ok("landlock abi 7"));
    let both = "a step can go without network (network namespace); a step can write only where it may (landlock abi 7)";
    assert_eq!(
        said(None, &full),
        (Status::Ok, format!("no step asks for one; {both}"))
    );
    let asking = flow(&[
        "{ network = false }",
        "{ network = true, write = [\"out\"] }",
    ]);
    assert_eq!(
        said(Some(&asking), &full),
        (Status::Ok, format!("s0, s1: enforced; {both}"))
    );
}

#[test]
fn what_a_host_lacks_is_a_note_until_a_step_needs_it() {
    let no_landlock = support(Ok("network namespace"), Err(LANDLOCK_2));
    let (status, detail) = said(Some(&flow(&[])), &no_landlock);
    assert_eq!(status, Status::Note);
    assert!(
        detail.ends_with(&format!(
            "a step cannot write only where it may: {LANDLOCK_2}"
        )),
        "{detail}"
    );
    // A step that only goes without network still runs.
    let (status, _) = said(Some(&flow(&["{ network = false }"])), &no_landlock);
    assert_eq!(status, Status::Ok);
    // One that is held to its writes does not.
    assert_eq!(
        said(Some(&flow(&["{ network = false }", "{ network = true, write = [\"out\"] }"])), &no_landlock),
        (
            Status::Fail,
            format!("s1 cannot be held to their writes here: {LANDLOCK_2}; a step that asked to be confined will not run")
        )
    );

    let no_userns = support(Err(NO_USERNS), Ok("landlock abi 4"));
    let (status, detail) = said(
        Some(&flow(&["{ network = false, write = [\"out\"] }"])),
        &no_userns,
    );
    assert_eq!(status, Status::Fail);
    assert!(
        detail.starts_with(&format!("s0 cannot go without network here: {NO_USERNS}")),
        "{detail}"
    );
}

#[test]
fn a_host_that_confines_nothing_says_so_once() {
    let why = "flot confines steps on Linux and macOS only, and this is windows";
    let none = support(Err(why), Err(why));
    assert_eq!(
        said(None, &none),
        (
            Status::Note,
            format!("no step asks for one; steps cannot be confined here: {why}")
        )
    );
}

/// On this host the check is the probes' own answer.
#[test]
fn the_host_running_the_suite_is_probed() {
    let dir = tempfile::tempdir().unwrap();
    let here = Support::here(dir.path());
    let (_, detail) = said(None, &here);
    assert!(detail.starts_with("no step asks for one; "), "{detail}");
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert!(detail.contains("steps cannot be confined here"), "{detail}");
    if let Ok(mechanism) = &here.network {
        assert!(
            detail.contains(&format!("go without network ({mechanism})")),
            "{detail}"
        );
    }
}
