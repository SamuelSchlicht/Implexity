// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{Value, json};

use crate::error::{AResult, AuthoringError};
use crate::py::jf;
use crate::sync::lock;

fn tx_err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::runtime("InteractionTransactionError", message)
}

#[derive(Clone, Debug)]
pub struct InteractionTransaction<S: Clone> {
    pub transaction_id: String,
    pub kind: String,
    pub base_revision: String,
    pub snapshot: S,
    pub metadata: Value,
    pub created: Instant,
    pub latest_requested: i64,
    pub latest_applied: i64,
    pub preview_state: S,
    pub closed: bool,
}

#[derive(Debug)]
pub struct InteractionTransactionManager<S: Clone> {
    pub timeout_seconds: f64,
    transactions: Mutex<HashMap<String, InteractionTransaction<S>>>,
}

fn token_urlsafe(n: usize) -> String {
    let bytes = implexity_io::atomic::os_random_bytes(n)
        .unwrap_or_else(|_| crate::py::token_hex(n).into_bytes()[..n].to_vec());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

impl<S: Clone> InteractionTransactionManager<S> {
    #[must_use]
    pub fn new(timeout_seconds: f64) -> Self {
        Self { timeout_seconds, transactions: Mutex::new(HashMap::new()) }
    }

    fn open<'a>(
        &self,
        map: &'a mut HashMap<String, InteractionTransaction<S>>,
        transaction_id: &str,
    ) -> AResult<&'a mut InteractionTransaction<S>> {
        let expired = match map.get(transaction_id) {
            None => return Err(tx_err("unknown or expired interaction transaction")),
            Some(tx) if tx.closed => return Err(tx_err("interaction transaction is already closed")),
            Some(tx) => tx.created.elapsed().as_secs_f64() > self.timeout_seconds,
        };
        if expired {
            map.remove(transaction_id);
            return Err(tx_err("interaction transaction expired"));
        }
        map.get_mut(transaction_id).ok_or_else(|| tx_err("unknown or expired interaction transaction"))
    }

    fn cleanup_locked(&self, map: &mut HashMap<String, InteractionTransaction<S>>) -> usize {
        let before = map.len();
        let timeout = self.timeout_seconds;
        map.retain(|_, tx| !(tx.closed || tx.created.elapsed().as_secs_f64() > timeout));
        before - map.len()
    }

    fn describe_tx(tx: &InteractionTransaction<S>) -> Value {
        json!({
            "transaction_id": tx.transaction_id,
            "kind": tx.kind,
            "base_revision": tx.base_revision,
            "latest_requested": tx.latest_requested,
            "latest_applied": tx.latest_applied,
            "metadata": tx.metadata,
            "age_seconds": jf(tx.created.elapsed().as_secs_f64().max(0.0)),
        })
    }


    pub fn begin(&self, kind: &str, base_revision: &str, snapshot: S, metadata: Value) -> AResult<Value> {
        if kind.is_empty() || base_revision.is_empty() {
            return Err(tx_err("kind and base_revision are required"));
        }
        let mut map = lock(&self.transactions);
        self.cleanup_locked(&mut map);
        let id = format!("ix_{}", token_urlsafe(18));
        let metadata = if metadata.is_object() { metadata } else { json!({}) };
        let tx = InteractionTransaction {
            transaction_id: id.clone(),
            kind: kind.to_string(),
            base_revision: base_revision.to_string(),
            preview_state: snapshot.clone(),
            snapshot,
            metadata,
            created: Instant::now(),
            latest_requested: -1,
            latest_applied: -1,
            closed: false,
        };
        let d = Self::describe_tx(&tx);
        map.insert(id, tx);
        Ok(d)
    }


    pub fn prime(&self, transaction_id: &str, state: S, sequence: i64) -> AResult<Value> {
        let mut map = lock(&self.transactions);
        let tx = self.open(&mut map, transaction_id)?;
        if tx.latest_requested >= 0 {
            return Err(AuthoringError::runtime(
                "OutOfOrderPreview",
                "interaction transaction is already primed",
            ));
        }
        tx.latest_requested = sequence;
        tx.latest_applied = sequence;
        tx.preview_state = state;
        Ok(Self::describe_tx(tx))
    }


    pub fn preview(
        &self,
        transaction_id: &str,
        sequence: i64,
        payload: &Value,
        apply: &mut dyn FnMut(S, &Value, &Value) -> AResult<S>,
    ) -> AResult<(Value, S, bool)> {
        let mut map = lock(&self.transactions);
        let tx = self.open(&mut map, transaction_id)?;
        if sequence <= tx.latest_requested {
            return Err(AuthoringError::runtime(
                "OutOfOrderPreview",
                format!("preview sequence {sequence} is not newer than {}", tx.latest_requested),
            ));
        }
        tx.latest_requested = sequence;
        let metadata = tx.metadata.clone();
        let candidate = apply(tx.snapshot.clone(), payload, &metadata)?;
        if sequence == tx.latest_requested {
            tx.preview_state = candidate;
            tx.latest_applied = sequence;
        }
        Ok((Self::describe_tx(tx), tx.preview_state.clone(), sequence == tx.latest_applied))
    }


    pub fn commit<R>(
        &self,
        transaction_id: &str,
        live_revision: &str,
        expected_sequence: Option<i64>,
        commit: &mut dyn FnMut(S, &InteractionTransaction<S>) -> AResult<R>,
    ) -> AResult<R> {
        let mut map = lock(&self.transactions);
        let tx = self.open(&mut map, transaction_id)?;
        if live_revision != tx.base_revision {
            return Err(AuthoringError::runtime(
                "RevisionConflict",
                format!(
                    "document changed during gesture: expected {}, got {live_revision}",
                    tx.base_revision
                ),
            ));
        }
        let expected = expected_sequence.unwrap_or(tx.latest_applied);
        if tx.latest_requested != tx.latest_applied || expected != tx.latest_applied {
            return Err(AuthoringError::runtime(
                "PreviewNotAcknowledged",
                format!(
                    "commit must acknowledge the exact latest applied preview: expected {expected}, requested {}, applied {}",
                    tx.latest_requested, tx.latest_applied
                ),
            ));
        }
        let snapshot_tx = tx.clone();
        let result = commit(tx.preview_state.clone(), &snapshot_tx)?;
        tx.closed = true;
        map.remove(transaction_id);
        Ok(result)
    }


    pub fn cancel(&self, transaction_id: &str) -> AResult<S> {
        let mut map = lock(&self.transactions);
        let tx = self.open(&mut map, transaction_id)?;
        let snapshot = tx.snapshot.clone();
        tx.closed = true;
        map.remove(transaction_id);
        Ok(snapshot)
    }


    pub fn describe(&self, transaction_id: &str) -> AResult<Value> {
        let mut map = lock(&self.transactions);
        let tx = self.open(&mut map, transaction_id)?;
        Ok(Self::describe_tx(tx))
    }


    pub fn get(&self, transaction_id: &str) -> AResult<InteractionTransaction<S>> {
        let mut map = lock(&self.transactions);
        Ok(self.open(&mut map, transaction_id)?.clone())
    }

    pub fn cleanup(&self) -> usize {
        let mut map = lock(&self.transactions);
        self.cleanup_locked(&mut map)
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        let mut map = lock(&self.transactions);
        self.cleanup_locked(&mut map);
        map.len()
    }
}
