//! Short-lived outcomes for writes handed to the UI. A socket timeout only
//! ends the caller's wait; it does not cancel the envelope.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::protocol::Response;

const RETAINED_OUTCOMES: usize = 1024;
const RETENTION_MS: u128 = 60 * 60 * 1000;

#[derive(Clone)]
struct Outcome {
    state: &'static str,
    phase: &'static str,
    started_at_ms: u128,
    updated_at_ms: u128,
    finished_at_ms: Option<u128>,
    revision: Option<u64>,
    reason: Option<String>,
    background_job: Option<&'static str>,
}

#[derive(Clone)]
struct Retry {
    command: Value,
    response: Option<Response>,
    finished_at_ms: Option<u128>,
}

#[derive(Default)]
struct Registry {
    outcomes: HashMap<String, Outcome>,
    order: VecDeque<String>,
    retries: HashMap<String, Retry>,
    retry_order: VecDeque<String>,
}

static REGISTRY: OnceLock<(Mutex<Registry>, Condvar)> = OnceLock::new();

/// A handler panic must settle its reservation. Replaying an error is safer
/// than running the same write again when it may already have reached the UI.
struct RetryGuard {
    key: String,
    id: u64,
    settled: bool,
}

impl Drop for RetryGuard {
    fn drop(&mut self) {
        if self.settled { return; }
        let (lock, wake) = registry();
        let mut registry = lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = registry.retries.get_mut(&self.key) {
            if entry.response.is_none() {
                entry.response = Some(Response::err(self.id, "request stopped unexpectedly; inspect Klipp before sending a new write"));
                entry.finished_at_ms = Some(now_ms());
            }
        }
        wake.notify_all();
    }
}

fn registry() -> &'static (Mutex<Registry>, Condvar) {
    REGISTRY.get_or_init(|| (Mutex::new(Registry::default()), Condvar::new()))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(crate) fn queued(background_job: Option<&'static str>) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let (lock, _) = registry();
    let mut registry = lock.lock().unwrap_or_else(|e| e.into_inner());
    registry.outcomes.insert(
        id.clone(),
        Outcome {
            state: "queued",
            phase: "waiting_for_ui",
            started_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
            revision: None,
            reason: None,
            background_job,
        },
    );
    registry.order.push_back(id.clone());
    prune(&mut registry);
    id
}

pub(crate) fn finished(id: &str, revision: u64, reason: Option<String>) {
    let (lock, _) = registry();
    let mut registry = lock.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(outcome) = registry.outcomes.get_mut(id) {
        let now = now_ms();
        outcome.state = if reason.is_some() {
            "refused"
        } else {
            "applied"
        };
        outcome.phase = if reason.is_some() {
            "ui_refused"
        } else {
            "ui_applied"
        };
        outcome.updated_at_ms = now;
        outcome.finished_at_ms = Some(now);
        outcome.revision = Some(revision);
        outcome.reason = reason;
    }
    prune(&mut registry);
}

pub(crate) fn status(id: &str) -> Result<Value, String> {
    let (lock, _) = registry();
    let mut registry = lock.lock().unwrap_or_else(|e| e.into_inner());
    prune(&mut registry);
    let outcome = registry
        .outcomes
        .get(id)
        .ok_or_else(|| format!("operation {id} is unknown or has expired"))?;
    Ok(json!({
        "operation_id": id,
        "state": outcome.state,
        "phase": outcome.phase,
        "started_at_ms": outcome.started_at_ms,
        "updated_at_ms": outcome.updated_at_ms,
        "finished_at_ms": outcome.finished_at_ms,
        "revision": outcome.revision,
        "reason": outcome.reason,
        "progress": null,
        "completion_scope": "ui_command",
        "background_job": outcome.background_job,
        "note": outcome.background_job.map(|_| "UI acknowledgement does not mean the background job has finished"),
    }))
}

/// Reserve the key before dispatch. Concurrent duplicates wait for the first
/// reply and then replay it with their own JSON-RPC id. A key used for a
/// different command is an error, not a way to hide a second edit.
pub(crate) fn with_idempotency_key(
    key: &str,
    command: Value,
    id: u64,
    run: impl FnOnce() -> Response,
) -> Response {
    let (lock, wake) = registry();
    let mut registry = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(existing) = registry.retries.get(key) {
            if existing.command != command {
                return Response::err(
                    id,
                    "idempotency key was already used for a different command",
                );
            }
            if let Some(mut response) = existing.response.clone() {
                response.id = id;
                return response;
            }
            registry = wake.wait(registry).unwrap_or_else(|e| e.into_inner());
        } else {
            registry.retries.insert(
                key.to_owned(),
                Retry {
                    command,
                    response: None,
                    finished_at_ms: None,
                },
            );
            registry.retry_order.push_back(key.to_owned());
            break;
        }
    }
    drop(registry);
    let mut guard = RetryGuard { key: key.to_owned(), id, settled: false };
    let response = run();
    let mut registry = lock.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = registry.retries.get_mut(key) {
        entry.response = Some(response.clone());
        entry.finished_at_ms = Some(now_ms());
    }
    prune(&mut registry);
    wake.notify_all();
    guard.settled = true;
    response
}

fn prune(registry: &mut Registry) {
    let cutoff = now_ms().saturating_sub(RETENTION_MS);
    let expired: Vec<_> = registry
        .order
        .iter()
        .filter(|id| {
            registry
                .outcomes
                .get(*id)
                .and_then(|o| o.finished_at_ms)
                .is_some_and(|at| at < cutoff)
        })
        .cloned()
        .collect();
    for id in expired {
        registry.outcomes.remove(&id);
        registry.order.retain(|entry| entry != &id);
    }
    let expired_retries: Vec<_> = registry
        .retry_order
        .iter()
        .filter(|key| {
            registry
                .retries
                .get(*key)
                .and_then(|r| r.finished_at_ms)
                .is_some_and(|at| at < cutoff)
        })
        .cloned()
        .collect();
    for key in expired_retries {
        registry.retries.remove(&key);
        registry.retry_order.retain(|entry| entry != &key);
    }
    while registry.order.len() > RETAINED_OUTCOMES {
        let Some(position) = registry.order.iter().position(|id| {
            registry
                .outcomes
                .get(id)
                .is_some_and(|o| o.finished_at_ms.is_some())
        }) else {
            break;
        };
        let id = registry.order.remove(position).unwrap();
        registry.outcomes.remove(&id);
    }
    while registry.retry_order.len() > RETAINED_OUTCOMES {
        let Some(position) = registry.retry_order.iter().position(|key| {
            registry
                .retries
                .get(key)
                .is_some_and(|r| r.response.is_some())
        }) else {
            break;
        };
        let key = registry.retry_order.remove(position).unwrap();
        registry.retries.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_first_run_settles_the_key_for_later_retries() {
        let key = format!("panic-{}", uuid::Uuid::new_v4());
        let command = json!({ "method": "apply", "ops": [] });
        let panicked = std::panic::catch_unwind(|| {
            with_idempotency_key(&key, command.clone(), 1, || panic!("test handler panic"));
        });
        assert!(panicked.is_err());
        let replay = with_idempotency_key(&key, command, 2, || panic!("must not run again"));
        assert_eq!(replay.id, 2);
        assert!(!replay.ok);
        assert!(replay.error.unwrap().contains("stopped unexpectedly"));
    }
}
