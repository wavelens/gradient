/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The NixOS VM integration test cannot speak binary frames from Python.
//! This JSONL harness is driving a real subprocess through [`EvalWorker`] to cover both
//! sides of the wire.

use anyhow::{Context, Result};
use gradient_util::sync::Mutex;
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

use super::transport::EvalWorker;
use gradient_eval::ipc::EvalRequest;

pub async fn run_eval_driver(requests_path: &str, eval_cache_dir: &str) -> Result<i32> {
    let text = tokio::fs::read_to_string(requests_path)
        .await
        .with_context(|| format!("reading eval driver requests from {requests_path}"))?;

    let live = Arc::new(Mutex::new(HashSet::new()));
    let mut worker = EvalWorker::spawn(eval_cache_dir, live, Arc::default())
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
                input_overrides,
            } => worker
                .plan(repository, wildcards, input_overrides)
                .await
                .map(|(shards, errors)| json!({"kind": "plan_ok", "shards": shards, "errors": errors})),
            EvalRequest::List {
                repository,
                wildcards,
                only,
                input_overrides,
            } => worker
                .list(repository, wildcards, only, input_overrides)
                .await
                .map(|l| {
                    json!({
                        "kind": "list_ok",
                        "attrs": l.attrs,
                        "deferred": l.deferred,
                        "warnings": l.warnings,
                        "errors": l.errors,
                    })
                }),
            EvalRequest::Resolve {
                repository,
                attrs,
                input_overrides,
            } => {
                let (items, end) = worker.resolve(repository, attrs, input_overrides).await;
                end.map(|(warnings, _stats)| {
                    json!({"kind": "resolve_ok", "items": items, "warnings": warnings})
                })
            }
            EvalRequest::Fingerprint {
                repository,
                input_overrides,
            } => worker
                .fingerprint(repository, input_overrides)
                .await
                .map(|fingerprint| json!({"kind": "fingerprint_ok", "fingerprint": fingerprint})),
            EvalRequest::Checkpoint {
                repository,
                input_overrides,
            } => worker
                .checkpoint(repository, input_overrides)
                .await
                .map(|()| json!({"kind": "checkpoint_ok"})),
            EvalRequest::FetchInput { .. } => {
                Err(anyhow::anyhow!("the eval driver does not fetch inputs"))
            }
        };

        match output {
            Ok(v) => println!("{v}"),
            Err(e) => println!("{}", json!({"kind": "err", "message": format!("{e:#}")})),
        }
    }

    worker.shutdown().await;
    Ok(0)
}
