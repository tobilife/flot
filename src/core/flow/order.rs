//! The order a flow's steps run in, and the steps one step needs, directly
//! or through others.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};

use super::Flow;

impl Flow {
    /// Dependency order. Ties break on step id so the order — and therefore the
    /// run root digest — does not depend on declaration order or host threading.
    pub fn topo_order(&self) -> Result<Vec<String>> {
        let mut pending: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for step in &self.steps {
            pending.insert(
                step.id.as_str(),
                step.needs.iter().map(String::as_str).collect(),
            );
        }
        let mut order = Vec::with_capacity(self.steps.len());
        while !pending.is_empty() {
            let ready: Vec<&str> = pending
                .iter()
                .filter(|(_, needs)| needs.is_empty())
                .map(|(id, _)| *id)
                .collect();
            if ready.is_empty() {
                let mut stuck: Vec<&str> = pending.keys().copied().collect();
                stuck.sort();
                bail!("dependency cycle among steps: {}", stuck.join(", "));
            }
            for id in ready {
                pending.remove(id);
                for needs in pending.values_mut() {
                    needs.remove(id);
                }
                order.push(id.to_string());
            }
        }
        Ok(order)
    }

    /// Transitive dependency closure of a step, in dependency order.
    pub fn closure(&self, id: &str) -> Result<Vec<String>> {
        if self.step(id).is_none() {
            bail!("unknown step `{}`", id);
        }
        let wanted: BTreeSet<String> = {
            let mut acc = BTreeSet::new();
            let mut stack = vec![id.to_string()];
            while let Some(current) = stack.pop() {
                if !acc.insert(current.clone()) {
                    continue;
                }
                let step = self.step(&current).expect("validated needs");
                stack.extend(step.needs.iter().cloned());
            }
            acc
        };
        Ok(self
            .topo_order()?
            .into_iter()
            .filter(|candidate| wanted.contains(candidate))
            .collect())
    }
}
