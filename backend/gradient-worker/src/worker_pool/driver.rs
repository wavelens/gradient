/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! JSONL harness for the eval-worker IPC, behind the hidden `--eval-driver`
//! flag. Reads [`EvalRequest`]s as JSON lines, drives one real subprocess over
//! the production rkyv transport (spawn, handshake, frames, streamed resolve),
//! and prints one JSON response line per request. Exists for the NixOS VM
//! integration test, which cannot speak binary frames from Python; going
//! through [`EvalWorker`] means the test covers both sides of the wire.

use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::transport::EvalWorker;
use gradient_eval::ipc::EvalRequest;

pub async fn run_eval_driver(requests_path: &str, eval_cache_dir: &str) -> Result<i32> {
    let text = tokio::fs::read_to_string(requests_path)
        .await
        .with_context(|| format!("reading eval driver requests from {requests_path}"))?;

    let live = Arc::new(Mutex::new(HashSet::new()));
    let mut worker = EvalWorker::spawn(eval_cache_dir, live)
        .await
        .context("spawning eval worker for driver")?;

    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let req: EvalRequest =
            serde_json::from_str(line).with_context(|| format!("parsing request: {line}"))?;

        let output = match req {
            EvalRequest::Shutdown => {
                worker.shutdown().await;
                return Ok(0);
            }
            EvalRequest::Plan {
                repository,
                wildcards,
            } => worker
                .plan(repository, wildcards)
                .await
                .map(|sub_patterns| json!({"kind": "plan_ok", "sub_patterns": sub_patterns})),
            EvalRequest::List {
                repository,
                wildcards,
            } => worker
                .list(repository, wildcards)
                .await
                .map(|(attrs, warnings, _stats)| {
                    json!({"kind": "list_ok", "attrs": attrs, "warnings": warnings})
                }),
            EvalRequest::Resolve { repository, attrs } => {
                let (items, end) = worker.resolve(repository, attrs).await;
                end.map(|(warnings, _stats)| {
                    json!({"kind": "resolve_ok", "items": items, "warnings": warnings})
                })
            }
            EvalRequest::Fingerprint { repository } => worker
                .fingerprint(repository)
                .await
                .map(|fingerprint| json!({"kind": "fingerprint_ok", "fingerprint": fingerprint})),
            EvalRequest::Checkpoint { repository } => worker
                .checkpoint(repository)
                .await
                .map(|()| json!({"kind": "checkpoint_ok"})),
        };

        match output {
            Ok(v) => println!("{v}"),
            Err(e) => println!("{}", json!({"kind": "err", "message": format!("{e:#}")})),
        }
    }

    worker.shutdown().await;
    Ok(0)
}
