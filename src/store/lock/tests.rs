//! Tests of the store lock.

use super::super::tests::store;

#[test]
fn runs_write_to_the_store_side_by_side() {
    let (_dir, store) = store();
    let first = store.lock(false).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| sender.send(store.lock(false).map(drop)).unwrap());
        let second = receiver.recv_timeout(std::time::Duration::from_secs(10));
        // Let a second run that waited go, so the scope can end.
        drop(first);
        assert!(
            matches!(second, Ok(Ok(()))),
            "a second run waited for the first"
        );
    });
}
