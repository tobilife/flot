//! Tests of storing receipts.

use super::super::tests::{receipt, store};
use crate::core::digest;
use crate::core::receipt::StepStatus;

#[test]
fn tampered_receipts_are_refused_at_write_time() {
    let (_dir, store) = store();
    let mut tampered = receipt(&digest::bytes(b"k"), b"out");
    tampered.identity.status = StepStatus::Failed;
    let err = store.put_receipt(&tampered).unwrap_err();
    assert!(err.to_string().contains("does not match its body"), "{err}");
}
