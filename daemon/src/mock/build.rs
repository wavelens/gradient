/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::journal::Violation;
use crate::mock::conn::{MockConn, err};
use crate::mock::spec::{Node, Outcome};
use crate::mock::store::{MockStore, Origin};
use crate::mock::{MockState, artefact, store_path, timing};
use harmonia_protocol::daemon::wire::types2::{
    BuildResult, BuildResultFailure, BuildResultInner, BuildResultSuccess, FailureStatus,
    SuccessStatus,
};
use harmonia_protocol::daemon::{DaemonResult, FutureResultExt as _, ResultLog};
use harmonia_protocol::log::{
    Activity, ActivityResult, ActivityType, Field, LogMessage, ResultType, StopActivity, Verbosity,
};
use harmonia_store_derivation::derivation::BasicDerivation;
use harmonia_store_derivation::derived_path::OutputName;
use harmonia_store_derivation::realisation::UnkeyedRealisation;
use harmonia_store_path::StorePath;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{Notify, mpsc};

const ACTIVITY: u64 = 1;

type Logs = mpsc::UnboundedSender<LogMessage>;
type BuiltOutputs = BTreeMap<OutputName, UnkeyedRealisation>;

pub fn build_derivation(
    conn: MockConn,
    drv_path: StorePath,
    drv: BasicDerivation,
) -> impl ResultLog<Output = DaemonResult<BuildResult>> + Send + 'static {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let paths = vec![format!("/nix/store/{drv_path}")];
        let timer = conn
            .state
            .journal
            .start(conn.conn.id, "build_derivation", paths);
        let result = run(&conn.state, &drv_path, &drv, &tx).await;
        let failure = match &result {
            Ok(r) => r
                .failure()
                .map(|f| String::from_utf8_lossy(&f.error_msg).into_owned()),
            Err(e) => Some(e.to_string()),
        };
        timer.finish(failure.is_none(), failure);
        result
    });
    let logs = async_stream::stream! {
        while let Some(msg) = rx.recv().await {
            yield msg;
        }
    };
    async move { task.await.map_err(err)? }.with_logs(logs)
}

pub fn release(state: &MockState, id: &str) {
    state.overrides.lock().expect("overrides").remove(id);
    hang(state, id).notify_one();
}

pub async fn run(
    state: &MockState,
    drv_path: &StorePath,
    drv: &BasicDerivation,
    logs: &Logs,
) -> DaemonResult<BuildResult> {
    let full = format!("/nix/store/{drv_path}");
    let Some((id, node)) = state.config.by_drv(&full) else {
        state
            .journal
            .violation(Violation::UnknownDerivation { drv: full });
        return Ok(failure(
            FailureStatus::MiscFailure,
            "mock: unknown derivation",
        ));
    };

    let missing = missing_inputs(state, drv);
    if !missing.is_empty() {
        state
            .journal
            .violation(Violation::BuildWithMissingInput { drv: full, missing });
        return Ok(failure(
            FailureStatus::MiscFailure,
            "mock: dependency is not valid",
        ));
    }

    let attempt = next_attempt(state, id);
    let outcome = state
        .overrides
        .lock()
        .expect("overrides")
        .get(id)
        .copied()
        .unwrap_or(node.build.outcome);
    let started = now_secs();
    let _running = Running::start(state, id);
    start_activity(logs, &full, node);
    if outcome == Outcome::Hang {
        hang(state, id).notified().await;
    }

    tokio::time::sleep(timing::build_delay(node, attempt)).await;
    let result = match outcome {
        Outcome::Fail => {
            let _ = logs.send(line("mock: build failed on purpose"));
            failure(
                fail_status(&node.build.fail_status),
                "mock: build failed on purpose",
            )
        }
        Outcome::Success | Outcome::Hang => {
            let built = register_outputs(state, id, node, drv_path).await?;
            success(built, started)
        }
    };
    let _ = logs.send(LogMessage::StopActivity(StopActivity { id: ACTIVITY }));
    Ok(result)
}

fn missing_inputs(state: &MockState, drv: &BasicDerivation) -> Vec<String> {
    drv.inputs
        .iter()
        .filter(|p| !state.store.is_valid(p).unwrap_or(false))
        .map(|p| format!("/nix/store/{p}"))
        .collect()
}

struct Running<'a> {
    state: &'a MockState,
    id: &'a str,
}

impl<'a> Running<'a> {
    fn start(state: &'a MockState, id: &'a str) -> Self {
        *state
            .running
            .lock()
            .expect("running")
            .entry(id.to_owned())
            .or_default() += 1;
        Self { state, id }
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut running = self.state.running.lock().expect("running");
        if let Some(count) = running.get_mut(self.id) {
            *count -= 1;
            if *count == 0 {
                running.remove(self.id);
            }
        }
    }
}

fn next_attempt(state: &MockState, id: &str) -> u32 {
    let mut attempts = state.attempts.lock().expect("attempts");
    let slot = attempts.entry(id.to_owned()).or_default();
    *slot += 1;
    *slot
}

fn start_activity(logs: &Logs, drv: &str, node: &Node) {
    let _ = logs.send(LogMessage::StartActivity(Activity {
        id: ACTIVITY,
        level: Verbosity::Info,
        activity_type: ActivityType::Build,
        text: format!("building '{drv}'").into(),
        fields: vec![],
        parent: 0,
    }));
    for text in &node.build.log {
        let _ = logs.send(line(text));
    }
}

async fn register_outputs(
    state: &MockState,
    id: &str,
    node: &Node,
    drv_path: &StorePath,
) -> DaemonResult<BuiltOutputs> {
    let mut built = BTreeMap::new();
    for (name, out) in &node.outputs {
        let path = store_path(&out.path).map_err(err)?;
        if state.store.is_valid(&path).map_err(err)? {
            state.journal.violation(Violation::RebuildOfValidOutput {
                drv: format!("/nix/store/{drv_path}"),
                output: name.clone(),
            });
        } else {
            let nar = artefact::render(id, node, name).map_err(err)?;
            let refs = out
                .references
                .iter()
                .map(|r| store_path(r))
                .collect::<anyhow::Result<_>>()
                .map_err(err)?;
            let info = MockStore::describe(&nar, refs, Some(drv_path.clone()), None);
            state
                .store
                .register(&path, &nar, info, Origin::Built)
                .await
                .map_err(err)?;
        }

        let realisation = UnkeyedRealisation {
            out_path: path,
            signatures: Default::default(),
        };
        built.insert(name.parse().map_err(err)?, realisation);
    }
    Ok(built)
}

fn fail_status(name: &str) -> FailureStatus {
    match name {
        "TransientFailure" => FailureStatus::TransientFailure,
        "TimedOut" => FailureStatus::TimedOut,
        "MiscFailure" => FailureStatus::MiscFailure,
        _ => FailureStatus::PermanentFailure,
    }
}

fn success(built_outputs: BuiltOutputs, started: i64) -> BuildResult {
    BuildResult {
        inner: BuildResultInner::Success(BuildResultSuccess {
            status: SuccessStatus::Built,
            built_outputs,
        }),
        times_built: 1,
        start_time: started,
        stop_time: now_secs(),
        cpu_user: None,
        cpu_system: None,
    }
}

fn failure(status: FailureStatus, msg: &str) -> BuildResult {
    BuildResult {
        inner: BuildResultInner::Failure(BuildResultFailure {
            status,
            error_msg: msg.as_bytes().to_vec(),
            is_non_deterministic: false,
        }),
        times_built: 0,
        start_time: 0,
        stop_time: 0,
        cpu_user: None,
        cpu_system: None,
    }
}

fn line(text: &str) -> LogMessage {
    LogMessage::Result(ActivityResult {
        fields: vec![Field::String(text.to_owned().into())],
        id: ACTIVITY,
        result_type: ResultType::BuildLogLine,
    })
}

fn hang(state: &MockState, id: &str) -> Arc<Notify> {
    state
        .hangs
        .lock()
        .expect("hangs")
        .entry(id.to_owned())
        .or_insert_with(|| Arc::new(Notify::new()))
        .clone()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::spec::DaemonConfig;
    use crate::mock::{MockBackend, seed_node};
    use crate::server::{next_conn, serve_stream};
    use harmonia_protocol::daemon::DaemonStore as _;
    use harmonia_protocol::daemon::wire::types2::BuildMode;
    use harmonia_store_derivation::derivation::DerivationOutput;
    use harmonia_store_path::StorePathSet;
    use harmonia_store_remote::DaemonClient;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    type Client = DaemonClient<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

    fn config() -> DaemonConfig {
        DaemonConfig::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.json"),
        )
        .expect("config")
    }

    async fn setup() -> (Arc<MockBackend>, Client, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = MockBackend::new(config(), dir.path().to_path_buf(), None)
            .await
            .expect("backend");
        let (client_side, server_side) = tokio::io::duplex(1 << 20);
        serve_stream(&backend, next_conn(None), server_side);
        let (r, w) = tokio::io::split(client_side);
        let client = DaemonClient::builder()
            .connect(r, w)
            .await
            .expect("handshake");
        (backend, client, dir)
    }

    async fn seed(backend: &MockBackend, id: &str) {
        seed_node(&backend.0, id).await.expect("seed");
    }

    fn node_path(id: &str) -> StorePath {
        store_path(&config().derivations[id].drv_path).expect("drv")
    }

    fn node_out(id: &str) -> StorePath {
        store_path(&config().derivations[id].outputs["out"].path).expect("out")
    }

    fn basic(id: &str, inputs: StorePathSet) -> BasicDerivation {
        let node = &config().derivations[id];
        BasicDerivation {
            name: node.name.parse().expect("name"),
            outputs: BTreeMap::from([(
                "out".parse().expect("out"),
                DerivationOutput::InputAddressed(node_out(id)),
            )]),
            inputs,
            platform: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            env: BTreeMap::new(),
            structured_attrs: None,
        }
    }

    fn app_basic() -> BasicDerivation {
        basic("t/app", StorePathSet::from([node_out("t/lib")]))
    }

    fn lib_basic() -> BasicDerivation {
        basic("t/lib", StorePathSet::new())
    }

    #[tokio::test]
    async fn builds_after_inputs_are_valid_and_registers_outputs() {
        let (backend, mut client, _dir) = setup().await;
        seed(&backend, "t/lib").await;
        let result = client
            .build_derivation(&node_path("t/app"), &app_basic(), BuildMode::Normal)
            .await
            .expect("build");
        assert!(result.success().is_some());
        let out = node_out("t/app");
        assert!(backend.0.store.is_valid(&out).expect("valid"));
        let info = backend.0.store.info(&out).expect("info").expect("some");
        assert_eq!(info.deriver, Some(node_path("t/app")));
        assert!(backend.0.journal.violations().is_empty());
    }

    #[tokio::test]
    async fn missing_input_fails_and_is_a_violation() {
        let (backend, mut client, _dir) = setup().await;
        let result = client
            .build_derivation(&node_path("t/app"), &app_basic(), BuildMode::Normal)
            .await
            .expect("rpc ok");
        assert!(result.success().is_none());
        assert!(matches!(
            backend.0.journal.violations()[0],
            Violation::BuildWithMissingInput { .. }
        ));
    }

    #[tokio::test]
    async fn unknown_derivation_fails_cleanly() {
        let (backend, mut client, _dir) = setup().await;
        let stray =
            StorePath::from_base_path(&format!("{}-stray.drv", "3".repeat(32))).expect("path");
        let result = client
            .build_derivation(&stray, &app_basic(), BuildMode::Normal)
            .await
            .expect("rpc ok");
        assert!(result.success().is_none());
        assert!(matches!(
            backend.0.journal.violations()[0],
            Violation::UnknownDerivation { .. }
        ));
        assert!(client.is_valid_path(&stray).await.is_ok());
    }

    #[tokio::test]
    async fn fail_outcome_returns_permanent_failure_without_outputs() {
        let (backend, mut client, _dir) = setup().await;
        seed(&backend, "t/lib").await;
        backend
            .0
            .overrides
            .lock()
            .expect("overrides")
            .insert("t/app".into(), Outcome::Fail);
        let result = client
            .build_derivation(&node_path("t/app"), &app_basic(), BuildMode::Normal)
            .await
            .expect("rpc ok");
        let failure = result.failure().expect("failure");
        assert_eq!(failure.status, FailureStatus::PermanentFailure);
        assert!(!backend.0.store.is_valid(&node_out("t/app")).expect("valid"));
    }

    #[tokio::test]
    async fn hang_waits_for_release() {
        let (backend, mut client, _dir) = setup().await;
        seed(&backend, "t/lib").await;
        backend
            .0
            .overrides
            .lock()
            .expect("overrides")
            .insert("t/app".into(), Outcome::Hang);
        let handle = tokio::spawn(async move {
            client
                .build_derivation(&node_path("t/app"), &app_basic(), BuildMode::Normal)
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!handle.is_finished());
        let running = backend.0.running.lock().expect("running").clone();
        assert_eq!(running, BTreeMap::from([("t/app".to_owned(), 1)]));
        release(&backend.0, "t/app");
        let result = handle.await.expect("join").expect("build");
        assert!(result.success().is_some());
        assert!(backend.0.running.lock().expect("running").is_empty());
    }

    #[tokio::test]
    async fn log_lines_arrive_as_build_log_results() {
        let (backend, mut client, _dir) = setup().await;
        let (drv_path, drv) = (node_path("t/lib"), lib_basic());
        let logs = client.build_derivation(&drv_path, &drv, BuildMode::Normal);
        let mut logs = std::pin::pin!(logs);
        let mut lines = Vec::new();
        while let Some(msg) = futures::StreamExt::next(&mut logs).await {
            if let LogMessage::Result(r) = msg {
                lines.push(r.fields.clone());
            }
        }
        assert!(logs.await.expect("build").success().is_some());
        assert_eq!(lines, vec![vec![Field::String("building lib".into())]]);
        assert!(backend.0.journal.violations().is_empty());
    }

    #[tokio::test]
    async fn rebuilding_a_valid_output_is_a_violation() {
        let (backend, mut client, _dir) = setup().await;
        seed(&backend, "t/lib").await;
        let result = client
            .build_derivation(&node_path("t/lib"), &lib_basic(), BuildMode::Normal)
            .await
            .expect("build");
        assert!(result.success().is_some());
        assert!(matches!(
            backend.0.journal.violations()[0],
            Violation::RebuildOfValidOutput { .. }
        ));
    }
}
