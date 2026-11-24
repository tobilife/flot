//! Bounded-concurrency DAG scheduling, independent of what a node does.
//!
//! A node launches only once every node it needs has settled, so the work for
//! a node can be prepared from its dependencies' exact results. Results are
//! returned keyed by node, never in completion order: callers assemble them in
//! dependency order, which is what keeps digests independent of `jobs`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc;

use anyhow::Result;

/// Run every node in `order`, at most `jobs` at a time.
///
/// `prepare` runs on the scheduling thread with every result settled so far and
/// turns a ready node into owned work; `work` runs that on a worker thread. The
/// first worker error stops new launches and is returned once running work
/// has finished.
pub fn run_dag<P, T, F, W>(
    order: &[String],
    needs: &BTreeMap<String, Vec<String>>,
    jobs: usize,
    mut prepare: F,
    work: W,
) -> Result<HashMap<String, T>>
where
    P: Send,
    T: Send,
    F: FnMut(&str, &HashMap<String, T>) -> P,
    W: Fn(P) -> Result<T> + Sync,
{
    let jobs = jobs.max(1);
    let work = &work;
    let mut settled: HashMap<String, T> = HashMap::new();
    let mut launched: HashSet<String> = HashSet::new();
    let (sender, receiver) = mpsc::channel::<(String, Result<T>)>();

    std::thread::scope(|scope| -> Result<()> {
        let mut inflight = 0usize;
        loop {
            while inflight < jobs {
                let Some(node) = next_ready(order, needs, &launched, &settled) else {
                    break;
                };
                let prepared = prepare(&node, &settled);
                launched.insert(node.clone());
                inflight += 1;
                let sender = sender.clone();
                scope.spawn(move || {
                    // A worker that panicked would never report, and the
                    // scheduler would wait for it forever.
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(prepared)))
                            .unwrap_or_else(|_| {
                                Err(anyhow::anyhow!("settling step `{node}` panicked"))
                            });
                    let _ = sender.send((node, result));
                });
            }
            if inflight == 0 {
                return Ok(());
            }
            let (node, result) = receiver.recv().expect("each worker reports exactly once");
            inflight -= 1;
            settled.insert(node, result?);
        }
    })?;

    Ok(settled)
}

/// First node in `order` that has not launched and whose needs have all
/// settled. Scanning `order` keeps a single-job run in the sequence a reader
/// expects from the flow file.
fn next_ready<T>(
    order: &[String],
    needs: &BTreeMap<String, Vec<String>>,
    launched: &HashSet<String>,
    settled: &HashMap<String, T>,
) -> Option<String> {
    order
        .iter()
        .find(|node| {
            !launched.contains(*node)
                && needs
                    .get(*node)
                    .map(|needs| needs.iter().all(|need| settled.contains_key(need)))
                    .unwrap_or(true)
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_worker_fails_the_run_instead_of_hanging_it() {
        let order = vec!["a".to_string(), "b".to_string()];
        let needs = BTreeMap::from([("a".to_string(), vec![]), ("b".to_string(), vec![])]);
        let result = run_dag(
            &order,
            &needs,
            2,
            |node, _: &HashMap<String, ()>| node.to_string(),
            |node| {
                if node == "a" {
                    panic!("boom");
                }
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("panicked"));
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    fn graph(edges: &[(&str, &[&str])]) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
        let order = edges.iter().map(|(node, _)| node.to_string()).collect();
        let needs = edges
            .iter()
            .map(|(node, needs)| {
                (
                    node.to_string(),
                    needs.iter().map(|need| need.to_string()).collect(),
                )
            })
            .collect();
        (order, needs)
    }

    #[test]
    fn a_node_is_prepared_only_after_its_needs_settled() {
        let (order, needs) = graph(&[("a", &[]), ("b", &["a"]), ("c", &["a", "b"])]);
        let results = run_dag(
            &order,
            &needs,
            4,
            |node, settled: &HashMap<String, Vec<String>>| {
                for need in &needs[node] {
                    assert!(settled.contains_key(need), "{node} prepared before {need}");
                }
                node.to_string()
            },
            |node| Ok(vec![node]),
        )
        .unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn concurrency_never_exceeds_the_job_limit() {
        let (order, needs) = graph(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[]), ("e", &[])]);
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        run_dag(
            &order,
            &needs,
            2,
            |node, _: &HashMap<String, ()>| node.to_string(),
            |_| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                running.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn one_job_runs_nodes_in_declared_order() {
        let (order, needs) = graph(&[("x", &[]), ("y", &[]), ("z", &[])]);
        let seen = Mutex::new(Vec::new());
        run_dag(
            &order,
            &needs,
            1,
            |node, _: &HashMap<String, ()>| node.to_string(),
            |node| {
                seen.lock().unwrap().push(node);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["x", "y", "z"]);
    }

    #[test]
    fn a_worker_error_is_returned_and_stops_new_launches() {
        let (order, needs) = graph(&[("bad", &[]), ("after", &["bad"])]);
        let launched_after = AtomicUsize::new(0);
        let error = run_dag(
            &order,
            &needs,
            1,
            |node, _: &HashMap<String, ()>| node.to_string(),
            |node| {
                if node == "bad" {
                    anyhow::bail!("bad broke");
                }
                launched_after.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("bad broke"));
        assert_eq!(launched_after.load(Ordering::SeqCst), 0);
    }
}
