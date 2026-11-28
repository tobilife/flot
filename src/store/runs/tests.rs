//! Tests of run ids: none can name a file outside the store.

use super::super::tests::store;

#[test]
fn a_run_id_can_never_name_a_file_outside_the_store() {
    let (_dir, store) = store();
    for id in ["../escape", "a/b", "", "..", "x.json"] {
        assert!(store.attestations(id).is_err(), "{id}");
        assert!(store.add_attestations(id, &[]).is_err(), "{id}");
    }
    assert!(store.attestations("1795496079152-c5fe4934ed4a").is_ok());
}
