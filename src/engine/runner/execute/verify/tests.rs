use super::finished_exit;
use crate::engine::exec::Outcome;

/// A proof that exited 0 on its own.
fn passed() -> Outcome {
    Outcome {
        exit_code: Some(0),
        signal: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
        timed_out: false,
        interrupted: false,
        output_overflow: false,
        strays_stopped: false,
        output_detached: false,
        escaped: Vec::new(),
        started_at_ms: 0,
        duration_ms: 0,
    }
}

/// The exit of a proof that ran to its end is what it shows; one stopped
/// for its output, or by a stop, has none, even when it exited as it was.
#[test]
fn only_a_proof_that_ran_to_its_end_has_an_exit_code() {
    assert_eq!(finished_exit(&passed()), Some(0));
    let cut = Outcome {
        output_overflow: true,
        ..passed()
    };
    assert_eq!(finished_exit(&cut), None);
    let stopped = Outcome {
        interrupted: true,
        ..passed()
    };
    assert_eq!(finished_exit(&stopped), None);
}
