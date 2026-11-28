//! The files earlier receipts of a step answer for that are still as those
//! receipts say the step left them.

use std::collections::BTreeMap;

use crate::core::flow::{is_contained, Step};
use crate::engine::keying;
use crate::engine::runner::Runner;

impl Runner<'_> {
    /// The files earlier receipts of `step` answer for that are still as
    /// those receipts say the step left them: the receipt this key would
    /// reuse, and the step's receipt in the newest run here. A run that finds
    /// that work already done (a file it writes already holds what it writes,
    /// one it deletes is already gone) changes nothing there, and must answer
    /// for it all the same. A file someone else has changed since is no
    /// longer the step's doing, and is left out.
    pub(in crate::engine::runner) fn still_in_place(
        &self,
        step: &Step,
        step_key: &str,
    ) -> BTreeMap<String, Option<String>> {
        let latest = self.latest.get_or_init(|| {
            self.store
                .resolve_run("latest")
                .ok()
                .filter(|run| run.flow == self.flow.meta.name)
                .map(|run| {
                    run.steps
                        .into_iter()
                        .map(|entry| (entry.step_id, entry.receipt_id))
                        .collect()
                })
                .unwrap_or_default()
        });
        let earlier = self
            .store
            .recall(step_key)
            .ok()
            .flatten()
            .map(|recalled| recalled.receipt)
            .into_iter()
            .chain(
                latest
                    .get(&step.id)
                    .and_then(|receipt_id| self.store.load_receipt(receipt_id).ok()),
            );
        let mut kept = BTreeMap::new();
        for receipt in earlier {
            if receipt.identity.flow != self.flow.meta.name || receipt.identity.step_id != step.id {
                continue;
            }
            for (path, left) in receipt.identity.changed {
                if is_contained(&path)
                    && keying::content_of(&self.root, &path).is_ok_and(|now| now == left)
                {
                    kept.insert(path, left);
                }
            }
        }
        kept
    }
}
