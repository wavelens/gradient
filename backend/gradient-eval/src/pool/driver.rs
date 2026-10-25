/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The NixOS VM integration test cannot speak binary frames from Python. `--eval-driver`: JSONL
//! requests through the real `EvalWorker` transport, both sides of the wire.

use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::ipc::EvalRequest;

use super::dependencies::RefuseDependencies;
use super::transport::{EvalWorker, SpawnConfig};

pub async fn run_eval_driver(
    requests_path: &str,
    eval_cache_dir: &str,
    eval_metrics: bool,
) -> Result<i32> {
    let text = tokio::fs::read_to_string(requests_path)
        .await
        .with_context(|| format!("read eval driver requests from {requests_path}"))?;

    let live = Arc::new(Mutex::new(HashSet::new()));
    let mut worker = EvalWorker::spawn(
        &SpawnConfig {
            cache_dir: eval_cache_dir.to_owned(),
            trace_dir: None,
            metrics: eval_metrics,
            fetch_through_parent: false,
        },
        live,
        Arc::default(),
    )
    .await
    .context("spawn eval worker for driver")?;

    let dependencies = RefuseDependencies("the eval driver");
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let req: EvalRequest =
            serde_json::from_str(line).with_context(|| format!("parse request: {line}"))?;

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
                .plan(repository, wildcards, input_overrides, &dependencies)
                .await
                .map(|(shards, errors)| json!({"kind": "plan_ok", "shards": shards, "errors": errors})),
            EvalRequest::List {
                repository,
                wildcards,
                only,
                input_overrides,
            } => worker
                .list(repository, wildcards, only, input_overrides, &dependencies)
                .await
                .map(|l| {
                    json!({
                        "kind": "list_ok",
                        "items": l.items,
                        "deferred": l.deferred,
                        "warnings": l.warnings,
                        "errors": l.errors,
                    })
                }),
            EvalRequest::Fingerprint {
                repository,
                input_overrides,
            } => worker
                .fingerprint(repository, input_overrides)
                .await
                .map(|fingerprint| json!({"kind": "fingerprint_ok", "fingerprint": fingerprint})),
            EvalRequest::FetchInput { .. } => {
                Err(anyhow::anyhow!("the eval driver does not fetch inputs"))
            }
            EvalRequest::BuildDone { .. } | EvalRequest::InputDone { .. } => Err(anyhow::anyhow!(
                "the eval driver answers requests to the parent itself"
            )),
        };

        match output {
            Ok(v) => println!("{v}"),
            Err(e) => println!("{}", json!({"kind": "err", "message": format!("{e:#}")})),
        }
    }

    worker.shutdown().await;
    Ok(0)
}
