//! The calls the server's worker has been given: a cancellation withdraws
//! the call it names, whether that call waits or another one runs.

use serde_json::json;

use super::super::calls::Calls;

#[test]
fn a_call_cancelled_while_it_waits_never_starts_and_the_others_do() {
    let calls = Calls::default();
    calls.queue(&json!(1));
    calls.queue(&json!(2));
    calls.cancel(&json!(1));
    assert!(calls.start(&json!(1)).is_none(), "call 1 was cancelled");
    assert!(calls.start(&json!(2)).is_some(), "call 2 was not cancelled");
    assert!(calls.finish(&json!(2)), "call 2 still wants its reply");
}

#[test]
fn a_call_still_waiting_behind_a_running_one_can_be_cancelled() {
    let calls = Calls::default();
    calls.queue(&json!(1));
    calls.queue(&json!(2));
    assert!(calls.start(&json!(1)).is_some());
    calls.cancel(&json!(2));
    assert!(calls.finish(&json!(1)), "call 1 still wants its reply");
    assert!(calls.start(&json!(2)).is_none(), "call 2 was cancelled");
}
