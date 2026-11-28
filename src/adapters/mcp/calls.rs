//! The tool calls the worker has been given, so that a cancellation reaches
//! the call it names, whether that call waits or runs.

use std::sync::Mutex;

use serde_json::Value;

use crate::service::StopSignal;

/// Tool calls the worker has been given: queued, running, or withdrawn.
#[derive(Default)]
pub(super) struct Calls {
    state: Mutex<CallState>,
}

#[derive(Default)]
struct CallState {
    queued: Vec<Value>,
    running: Option<(Value, StopSignal)>,
    withdrawn: Vec<Value>,
}

impl Calls {
    pub(super) fn queue(&self, id: &Value) {
        self.lock().queued.push(id.clone());
    }

    /// The call's stop signal, unless it was withdrawn while it waited.
    pub(super) fn start(&self, id: &Value) -> Option<StopSignal> {
        let mut state = self.lock();
        state.queued.retain(|queued| queued != id);
        if let Some(position) = state.withdrawn.iter().position(|withdrawn| withdrawn == id) {
            state.withdrawn.remove(position);
            return None;
        }
        let stop = StopSignal::default();
        state.running = Some((id.clone(), stop.clone()));
        Some(stop)
    }

    /// Whether the finished call still wants its reply.
    pub(super) fn finish(&self, id: &Value) -> bool {
        let mut state = self.lock();
        state.running = None;
        match state.withdrawn.iter().position(|withdrawn| withdrawn == id) {
            Some(position) => {
                state.withdrawn.remove(position);
                false
            }
            None => true,
        }
    }

    pub(super) fn cancel(&self, id: &Value) {
        let mut state = self.lock();
        if let Some((_, stop)) = state.running.as_ref().filter(|(running, _)| running == id) {
            stop.stop();
            state.withdrawn.push(id.clone());
        } else if state.queued.contains(id) {
            state.withdrawn.push(id.clone());
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CallState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
