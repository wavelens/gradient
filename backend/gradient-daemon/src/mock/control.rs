/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::mock::build::release;
use crate::mock::spec::Outcome;
use crate::mock::{MockState, seed_node, store_path};
use serde_json::{Value, json};
use std::sync::Arc;

const COMMANDS: &[&str] = &[
    "snapshot", "valid", "running", "outcome", "release", "seed", "forget",
];

fn arg<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing argument {key}"))
}

fn node<'a>(state: &MockState, args: &'a Value) -> anyhow::Result<&'a str> {
    let id = arg(args, "node")?;
    anyhow::ensure!(
        state.config.derivations.contains_key(id),
        "unknown node {id}"
    );
    Ok(id)
}

pub fn dispatch(state: &Arc<MockState>, cmd: &str, args: &Value) -> Option<anyhow::Result<Value>> {
    COMMANDS.contains(&cmd).then(|| run(state, cmd, args))
}

fn run(state: &Arc<MockState>, cmd: &str, args: &Value) -> anyhow::Result<Value> {
    match cmd {
        "snapshot" => Ok(serde_json::to_value(state.store.snapshot())?),
        "running" => Ok(serde_json::to_value(
            &*state.running.lock().expect("running"),
        )?),
        "valid" => {
            let path = store_path(arg(args, "path")?)?;
            Ok(json!(state.store.is_valid(&path)?))
        }
        "outcome" => {
            let id = node(state, args)?;
            let outcome: Outcome = serde_json::from_value(args["outcome"].clone())?;
            state
                .overrides
                .lock()
                .expect("overrides")
                .insert(id.to_owned(), outcome);
            Ok(Value::Null)
        }
        "release" => {
            release(state, node(state, args)?);
            Ok(Value::Null)
        }
        "seed" => {
            let id = node(state, args)?;
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(seed_node(state, id))
            })?;
            Ok(Value::Null)
        }
        "forget" => forget(state, args),
        other => anyhow::bail!("unknown mock command {other}"),
    }
}

fn forget(state: &MockState, args: &Value) -> anyhow::Result<Value> {
    let paths: Vec<String> = match args.get("path").and_then(Value::as_str) {
        Some(path) => vec![path.to_owned()],
        None => state.config.derivations[node(state, args)?]
            .outputs
            .values()
            .map(|o| o.path.clone())
            .collect(),
    };
    for path in &paths {
        state.store.forget(&store_path(path)?)?;
    }
    Ok(json!(paths))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockBackend;
    use crate::mock::spec::DaemonConfig;

    async fn backend(dir: &tempfile::TempDir) -> Arc<MockBackend> {
        let config = DaemonConfig::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.json"),
        )
        .expect("config");
        MockBackend::new(config, dir.path().to_path_buf(), None)
            .await
            .expect("backend")
    }

    fn call(backend: &MockBackend, cmd: &str, args: Value) -> anyhow::Result<Value> {
        dispatch(&backend.0, cmd, &args).expect("known command")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seed_then_snapshot_lists_seeded() {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = backend(&dir).await;
        call(&backend, "seed", json!({ "node": "t/lib" })).expect("seed");
        let snapshot = call(&backend, "snapshot", json!({})).expect("snapshot");
        let lib = &backend.0.config.derivations["t/lib"].outputs["out"].path;
        assert_eq!(snapshot[0][0], lib.trim_start_matches("/nix/store/"));
        assert_eq!(snapshot[0][1], "seeded");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forget_by_node_removes_all_outputs() {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = backend(&dir).await;
        call(&backend, "seed", json!({ "node": "t/lib" })).expect("seed");
        let forgotten = call(&backend, "forget", json!({ "node": "t/lib" })).expect("forget");
        let lib = &backend.0.config.derivations["t/lib"].outputs["out"].path;
        assert_eq!(forgotten, json!([lib]));
        let valid = call(&backend, "valid", json!({ "path": lib })).expect("valid");
        assert_eq!(valid, json!(false));
    }

    #[tokio::test]
    async fn outcome_override_parses() {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = backend(&dir).await;
        call(
            &backend,
            "outcome",
            json!({ "node": "t/app", "outcome": "fail" }),
        )
        .expect("outcome");
        let overrides = backend.0.overrides.lock().expect("overrides");
        assert_eq!(overrides.get("t/app"), Some(&Outcome::Fail));
    }

    #[tokio::test]
    async fn unknown_node_is_an_error() {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = backend(&dir).await;
        assert!(call(&backend, "release", json!({ "node": "t/nope" })).is_err());
        assert!(dispatch(&backend.0, "journal", &json!({})).is_none());
    }
}
