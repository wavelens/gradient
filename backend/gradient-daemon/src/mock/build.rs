/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::journal::Violation;
use crate::mock::conn::{MockConn, already_valid, err};
use crate::mock::spec::{Node, Outcome};
use crate::mock::store::{MockStore, Origin};
use crate::mock::{MockState, artefact, store_path, timing};
use harmonia_protocol::daemon::wire::types::Operation;
use harmonia_protocol::daemon::wire::types2::{
    BuildResult, BuildResultFailure, BuildResultInner, BuildResultSuccess, FailureStatus,
    KeyedBuildResult, SuccessStatus,
};
use harmonia_protocol::daemon::{DaemonResult, FutureResultExt as _, ResultLog};
use harmonia_protocol::log::{
    Activity, ActivityResult, ActivityType, Field, LogMessage, ResultType, StopActivity, Verbosity,
};
use harmonia_store_aterm::parse_derivation_aterm;
use harmonia_store_derivation::derived_path::{DerivedPath, OutputName, SingleDerivedPath};
use harmonia_store_derivation::realisation::UnkeyedRealisation;
use harmonia_store_path::{StoreDir, StorePath, StorePathSet};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinSet;

const ACTIVITY: u64 = 1;

type Logs = mpsc::UnboundedSender<LogMessage>;
type BuiltOutputs = BTreeMap<OutputName, UnkeyedRealisation>;

pub fn build_paths(
    conn: MockConn,
    paths: Vec<DerivedPath>,
) -> impl ResultLog<Output = DaemonResult<Vec<KeyedBuildResult>>> + Send + 'static {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut task = JoinSet::new();
    task.spawn(async move {
        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            let result = build_path(&conn, &path, &tx).await?;
            results.push(KeyedBuildResult { path, result });
        }
        Ok(results)
    });
    let logs = async_stream::stream! {
        while let Some(msg) = rx.recv().await {
            yield msg;
        }
    };
    async move {
        let joined = task.join_next().await.expect("build task was spawned");
        joined.map_err(err)?
    }
    .with_logs(logs)
}

async fn build_path(conn: &MockConn, path: &DerivedPath, logs: &Logs) -> DaemonResult<BuildResult> {
    let drv_path = match path {
        DerivedPath::Built { drv_path, .. } => match drv_path.as_ref() {
            SingleDerivedPath::Opaque(drv) => drv,
            SingleDerivedPath::Built { .. } => {
                return Err(conn.unimplemented(Operation::BuildPathsWithResults));
            }
        },
        DerivedPath::Opaque(p) if conn.state.store.is_valid(p).map_err(err)? => {
            return Ok(already_valid());
        }
        DerivedPath::Opaque(_) => return Err(conn.unimplemented(Operation::BuildPathsWithResults)),
    };

    let timer = conn.state.journal.start(
        conn.conn.id,
        "build_paths_with_results",
        vec![format!("/nix/store/{drv_path}")],
    );
    let result = if conn.outputs_valid(drv_path).map_err(err)? {
        Ok(already_valid())
    } else {
        run(&conn.state, drv_path, logs).await
    };
    let failure = match &result {
        Ok(r) => r
            .failure()
            .map(|f| String::from_utf8_lossy(&f.error_msg).into_owned()),
        Err(e) => Some(e.to_string()),
    };
    timer.finish(failure.is_none(), failure);
    result
}

pub fn release(state: &MockState, id: &str) {
    state.overrides.lock().expect("overrides").remove(id);
    hang(state, id).notify_one();
}

pub async fn run(
    state: &MockState,
    drv_path: &StorePath,
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

    let missing = missing_inputs(state, &inputs(state, drv_path).map_err(err)?);
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

fn inputs(state: &MockState, drv_path: &StorePath) -> anyhow::Result<StorePathSet> {
    let aterm = std::fs::read(state.store.real_path(drv_path))?;
    let name = drv_path.name().to_string();
    let name = name.strip_suffix(".drv").unwrap_or(&name).parse()?;
    let drv = parse_derivation_aterm(&StoreDir::default(), &aterm, name)?;
    drv.inputs
        .iter()
        .map(|input| input_path(state, input))
        .collect()
}

fn input_path(state: &MockState, input: &SingleDerivedPath) -> anyhow::Result<StorePath> {
    let (drv, output) = match input {
        SingleDerivedPath::Opaque(path) => return Ok(path.clone()),
        SingleDerivedPath::Built { drv_path, output } => match drv_path.as_ref() {
            SingleDerivedPath::Opaque(drv) => (drv, output.to_string()),
            SingleDerivedPath::Built { .. } => anyhow::bail!("mock: dynamic derivation input"),
        },
    };
    let (_, node) = state
        .config
        .by_drv(&format!("/nix/store/{drv}"))
        .ok_or_else(|| anyhow::anyhow!("mock: unknown input derivation {drv}"))?;
    let out = node
        .outputs
        .get(&output)
        .ok_or_else(|| anyhow::anyhow!("mock: {drv} has no output {output}"))?;
    store_path(&out.path)
}

fn missing_inputs(state: &MockState, inputs: &StorePathSet) -> Vec<String> {
    inputs
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
        memory_peak: None,
        io_read_bytes: None,
        io_write_bytes: None,
        oom_kills: None,
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
        memory_peak: None,
        io_read_bytes: None,
        io_write_bytes: None,
        oom_kills: None,
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
    use crate::mock::nar::{NarFile, encode};
    use crate::mock::spec::DaemonConfig;
    use crate::mock::{MockBackend, seed_node};
    use crate::server::{TestClient as Client, connect_duplex};
    use harmonia_protocol::daemon::DaemonStore as _;
    use harmonia_protocol::daemon::wire::types2::BuildMode;
    use harmonia_store_derivation::derived_path::OutputSpec;

    fn config() -> DaemonConfig {
        DaemonConfig::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.json"),
        )
        .expect("config")
    }

    async fn setup() -> (Arc<MockBackend>, Client, (tempfile::TempDir, JoinSet<()>)) {
        let dir = tempfile::tempdir().expect("tmp");
        let backend = MockBackend::new(config(), dir.path().to_path_buf(), None)
            .await
            .expect("backend");
        write_drv(&backend, "t/lib", &[]).await;
        write_drv(&backend, "t/app", &["t/lib"]).await;
        let (server, client) = connect_duplex(&backend).await;
        (backend, client, (dir, server))
    }

    async fn write_drv(backend: &MockBackend, id: &str, deps: &[&str]) {
        let input_drvs: Vec<String> = deps
            .iter()
            .map(|dep| format!("(\"{}\",[\"out\"])", config().derivations[*dep].drv_path))
            .collect();
        let aterm = format!(
            "Derive([(\"out\",\"{}\",\"\",\"\")],[{}],[],\"x86_64-linux\",\"/bin/sh\",[],[])",
            config().derivations[id].outputs["out"].path,
            input_drvs.join(",")
        );
        let file = NarFile {
            contents: aterm.into_bytes(),
            executable: false,
        };
        let nar = encode(&BTreeMap::from([(String::new(), file)]));
        let refs = deps.iter().map(|dep| node_path(dep)).collect();
        backend
            .0
            .store
            .register(
                &node_path(id),
                &nar,
                MockStore::describe(&nar, refs, None, None),
                Origin::Evaluated,
            )
            .await
            .expect("register drv");
    }

    fn hang(backend: &MockBackend, id: &str) {
        backend
            .0
            .overrides
            .lock()
            .expect("overrides")
            .insert(id.into(), Outcome::Hang);
    }

    fn running(backend: &MockBackend) -> BTreeMap<String, u32> {
        backend.0.running.lock().expect("running").clone()
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

    fn request(drv: StorePath) -> [DerivedPath; 1] {
        [DerivedPath::Built {
            drv_path: Arc::new(SingleDerivedPath::Opaque(drv)),
            outputs: OutputSpec::All,
        }]
    }

    async fn build(client: &mut Client, drv: StorePath) -> BuildResult {
        let mut results = client
            .build_paths_with_results(&request(drv), BuildMode::Normal)
            .await
            .expect("rpc ok");
        assert_eq!(results.len(), 1);
        results.remove(0).result
    }

    #[tokio::test]
    async fn builds_after_inputs_are_valid_and_registers_outputs() {
        let (backend, mut client, _dir) = setup().await;
        seed(&backend, "t/lib").await;
        let result = build(&mut client, node_path("t/app")).await;
        assert!(result.success().is_some());
        let out = node_out("t/app");
        assert!(backend.0.store.is_valid(&out).expect("valid"));
        let info = backend.0.store.info(&out).expect("info").expect("some");
        assert_eq!(info.deriver, Some(node_path("t/app")));
        assert!(backend.0.journal.violations().is_empty());
    }

    #[tokio::test]
    async fn an_input_named_by_the_stored_derivation_that_is_missing_fails_and_is_a_violation() {
        let (backend, mut client, _dir) = setup().await;
        let result = build(&mut client, node_path("t/app")).await;
        assert!(result.success().is_none());
        assert!(matches!(
            &backend.0.journal.violations()[0],
            Violation::BuildWithMissingInput { missing, .. }
                if *missing == vec![format!("/nix/store/{}", node_out("t/lib"))]
        ));
    }

    #[tokio::test]
    async fn unknown_derivation_fails_cleanly() {
        let (backend, mut client, _dir) = setup().await;
        let stray =
            StorePath::from_base_path(&format!("{}-stray.drv", "3".repeat(32))).expect("path");
        let result = build(&mut client, stray.clone()).await;
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
        let result = build(&mut client, node_path("t/app")).await;
        let failure = result.failure().expect("failure");
        assert_eq!(failure.status, FailureStatus::PermanentFailure);
        assert!(!backend.0.store.is_valid(&node_out("t/app")).expect("valid"));
    }

    #[tokio::test]
    async fn hang_waits_for_release() {
        let (backend, mut client, _dir) = setup().await;
        seed(&backend, "t/lib").await;
        hang(&backend, "t/app");
        let mut build_task = JoinSet::new();
        build_task.spawn(async move { build(&mut client, node_path("t/app")).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(build_task.try_join_next().is_none());
        assert_eq!(running(&backend), BTreeMap::from([("t/app".to_owned(), 1)]));
        release(&backend.0, "t/app");
        let result = build_task
            .join_next()
            .await
            .expect("spawned")
            .expect("join");
        assert!(result.success().is_some());
        assert!(running(&backend).is_empty());
    }

    #[tokio::test]
    async fn a_dropped_connection_aborts_its_build() {
        let (backend, mut client, (_dir, server)) = setup().await;
        seed(&backend, "t/lib").await;
        hang(&backend, "t/app");
        let paths = request(node_path("t/app"));
        let pending = client.build_paths_with_results(&paths, BuildMode::Normal);
        let pending = tokio::time::timeout(std::time::Duration::from_millis(100), pending).await;
        assert!(pending.is_err(), "a hanging build returned");
        assert_eq!(running(&backend), BTreeMap::from([("t/app".to_owned(), 1)]));
        drop(server);
        for _ in 0..100 {
            if running(&backend).is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!(
            "build still running after its connection closed: {:?}",
            running(&backend)
        );
    }

    #[tokio::test]
    async fn log_lines_arrive_as_build_log_results() {
        let (backend, mut client, _dir) = setup().await;
        let paths = request(node_path("t/lib"));
        let logs = client.build_paths_with_results(&paths, BuildMode::Normal);
        let mut logs = std::pin::pin!(logs);
        let mut lines = Vec::new();
        while let Some(msg) = futures::StreamExt::next(&mut logs).await {
            if let LogMessage::Result(r) = msg {
                lines.push(r.fields.clone());
            }
        }
        let results = logs.await.expect("build");
        assert!(results[0].result.success().is_some());
        assert_eq!(lines, vec![vec![Field::String("building lib".into())]]);
        assert!(backend.0.journal.violations().is_empty());
    }

    #[tokio::test]
    async fn a_valid_output_is_reported_already_valid_without_a_build() {
        let (backend, mut client, _dir) = setup().await;
        seed(&backend, "t/lib").await;
        let result = build(&mut client, node_path("t/lib")).await;
        assert_eq!(
            result.success().map(|s| s.status),
            Some(SuccessStatus::AlreadyValid)
        );
        assert!(running(&backend).is_empty());
        assert!(backend.0.attempts.lock().expect("attempts").is_empty());
        assert!(backend.0.journal.violations().is_empty());
    }
}
