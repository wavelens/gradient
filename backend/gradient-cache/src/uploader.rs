/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The background uploader. The `cached_path` rows with `confirmed = false` and
//! the files under `nar-staged/` are the queue: this actor drains what this
//! instance staged into the object store, confirms each row through the graph
//! actor, and reconciles the rest on its first pass at boot and on every tick.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use futures::StreamExt as _;
use gradient_core::ServerState;
use gradient_db::UnconfirmedPath;
use gradient_graph::Demotion;
use gradient_storage::StagedNars;
use gradient_util::supervision::{ChildCtx, ChildSpec, PassError, run_pass};
use gradient_util::sync::Mutex;
use ractor::{Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, info, warn};

pub const HEALTH_NAME: &str = "nar-uploader";
const UPLOAD_TICK: Duration = Duration::from_secs(60);
const UPLOAD_PASS_BUDGET: Duration = Duration::from_secs(600);
const UPLOAD_SCAN_LIMIT: u64 = 1000;
const BACKOFF_BASE: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(900);
/// A staged file younger than this may belong to a commit whose row is not
/// written yet, so the orphan sweep leaves it alone.
const ORPHAN_MIN_AGE: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Wake,
    Tick,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassReport {
    pub uploaded: u64,
    pub confirmed: u64,
    pub demoted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub orphans_removed: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub attempts: u32,
    pub retry_after: Instant,
}

impl Backoff {
    pub fn next(previous: Option<Backoff>, now: Instant) -> Backoff {
        let attempts = previous.map_or(1, |b| b.attempts + 1);
        let delay = BACKOFF_BASE
            .saturating_mul(1u32 << (attempts - 1).min(5))
            .min(BACKOFF_MAX);

        Backoff {
            attempts,
            retry_after: now + delay,
        }
    }
}

pub type BackoffMap = Mutex<HashMap<String, Backoff>>;

pub async fn run_upload_pass(
    state: &Arc<ServerState>,
    backoff: &BackoffMap,
    scope: Scope,
) -> anyhow::Result<PassReport> {
    let Some(staged) = state.nar_storage.staged() else {
        return Ok(PassReport::default());
    };

    let rows = gradient_db::unconfirmed_cached_paths(&state.worker_db, UPLOAD_SCAN_LIMIT)
        .await
        .context("list unconfirmed cached paths")?;
    let unconfirmed: HashSet<String> = rows.iter().map(|r| r.hash.clone()).collect();
    let now = Instant::now();
    let mut report = PassReport::default();
    let mut uploads = Vec::new();
    let mut absent = Vec::new();
    for row in rows {
        let backing_off = backoff
            .lock()
            .get(&row.hash)
            .is_some_and(|b| b.retry_after > now);
        if backing_off {
            report.skipped += 1;
            continue;
        }

        if staged.exists(&row.hash).await {
            uploads.push(row);
        } else {
            absent.push(row);
        }
    }

    let concurrency = state.config.storage.nar_upload_concurrency.max(1);
    let mut results = futures::stream::iter(uploads.into_iter().map(|row| {
        let state = Arc::clone(state);
        async move {
            let outcome = upload_one(&state, &row).await;
            (row.hash, outcome)
        }
    }))
    .buffer_unordered(concurrency);
    while let Some((hash, outcome)) = results.next().await {
        match outcome {
            Ok(()) => {
                backoff.lock().remove(&hash);
                report.uploaded += 1;
            }
            Err(e) => {
                let previous = backoff.lock().get(&hash).copied();
                let next = Backoff::next(previous, Instant::now());
                warn!(%hash, attempts = next.attempts, error = %e, "staged NAR upload failed; will retry");
                backoff.lock().insert(hash, next);
                report.failed += 1;
            }
        }
    }

    if scope == Scope::Tick {
        let grace = chrono::Duration::hours(state.config.storage.nar_upload_grace_hours.max(0));
        let cutoff = gradient_types::now() - grace;
        for row in absent {
            match reconcile_absent(state, &row, cutoff).await {
                Absent::Confirmed => report.confirmed += 1,
                Absent::Demoted => report.demoted += 1,
                Absent::Waiting => report.skipped += 1,
            }
        }

        report.orphans_removed = sweep_orphans(state, staged, &unconfirmed, ORPHAN_MIN_AGE).await?;
    }

    Ok(report)
}

async fn upload_one(state: &ServerState, row: &UnconfirmedPath) -> anyhow::Result<()> {
    let staged = state.nar_storage.staged().context("no staged store")?;
    let Some((_, file)) = staged.open(&row.hash).await? else {
        return Ok(());
    };

    state
        .nar_storage
        .put_reader(&row.hash, file)
        .await
        .context("upload staged NAR")?;
    let confirmed = state
        .graph
        .confirm_nar(&row.hash, &row.file_hash)
        .await
        .context("confirm cached path")?;
    if confirmed {
        staged.remove(&row.hash).await?;
    } else {
        debug!(hash = %row.hash, "cached path moved on during its upload; keeping the newer staged file");
    }

    Ok(())
}

enum Absent {
    Confirmed,
    Demoted,
    Waiting,
}

/// A row with no staged file here: confirm it when the object is in storage,
/// demote it once it is older than the grace, otherwise leave it to whichever
/// instance holds the file.
async fn reconcile_absent(
    state: &Arc<ServerState>,
    row: &UnconfirmedPath,
    cutoff: chrono::NaiveDateTime,
) -> Absent {
    match state.nar_storage.head_size(&row.hash).await {
        Ok(Some(size)) if size == row.file_size => {
            if let Err(e) = state.graph.confirm_nar(&row.hash, &row.file_hash).await {
                warn!(hash = %row.hash, error = %e, "confirm of a present object failed");
                return Absent::Waiting;
            }

            Absent::Confirmed
        }
        Ok(_) if row.created_at < cutoff => {
            let demotion = Demotion::MissingNar {
                hash: row.hash.clone(),
            };
            if let Err(e) = state.graph.demote(demotion).await {
                warn!(hash = %row.hash, error = %e, "demote of a lost staged NAR failed");
                return Absent::Waiting;
            }

            state.nar_storage.hot().invalidate(&row.hash);
            warn!(hash = %row.hash, "staged NAR lost before its upload; cached path demoted so the producer rebuilds");
            Absent::Demoted
        }
        Ok(_) => Absent::Waiting,
        Err(e) => {
            warn!(hash = %row.hash, error = %e, "could not check storage for an unconfirmed path");
            Absent::Waiting
        }
    }
}

/// Delete the staged files nothing is waiting for.
///
/// `page` is the work list this pass read, which [`UPLOAD_SCAN_LIMIT`] caps: a
/// staged file it does not name is only a *candidate*, and the table gets the
/// final word. Treating the page as the whole truth made a backlog deeper than
/// the page delete the files it had yet to upload, so those rows reached their
/// upload grace with neither a staged file nor an object and demoted - the
/// producer rebuilt, staged again, and was deleted again on the next pass.
async fn sweep_orphans(
    state: &Arc<ServerState>,
    staged: &StagedNars,
    page: &HashSet<String>,
    min_age: Duration,
) -> anyhow::Result<u64> {
    let candidates = orphan_candidates(staged, page, min_age).await?;
    let wanted = gradient_db::unconfirmed_hashes_among(&state.worker_db, &candidates)
        .await
        .context("resolve orphan candidates against the cache index")?;

    remove_orphans(staged, &candidates, &wanted).await
}

/// Staged hashes the work list does not name and that are older than `min_age`,
/// so a commit whose row is not written yet cannot own them.
async fn orphan_candidates(
    staged: &StagedNars,
    page: &HashSet<String>,
    min_age: Duration,
) -> anyhow::Result<Vec<String>> {
    let cutoff = std::time::SystemTime::now() - min_age;

    Ok(staged
        .list()
        .await?
        .into_iter()
        .filter(|(hash, modified)| !page.contains(hash) && *modified <= cutoff)
        .map(|(hash, _)| hash)
        .collect())
}

/// Remove every candidate no unconfirmed row still wants. Returns how many went.
pub async fn remove_orphans(
    staged: &StagedNars,
    candidates: &[String],
    wanted: &HashSet<String>,
) -> anyhow::Result<u64> {
    let mut removed = 0;
    for hash in candidates.iter().filter(|h| !wanted.contains(*h)) {
        staged.remove(hash).await?;
        removed += 1;
    }

    Ok(removed)
}

pub enum UploaderMsg {
    Wake,
    Tick,
}

struct Uploader;

struct UploaderState {
    state: Arc<ServerState>,
    ctx: ChildCtx,
    backoff: Arc<BackoffMap>,
    wakes: Arc<AtomicU64>,
    wakes_seen: u64,
}

impl Actor for Uploader {
    type Msg = UploaderMsg;
    type State = UploaderState;
    type Arguments = (Arc<ServerState>, ChildCtx);

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        (state, ctx): Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        let wakes = Arc::new(AtomicU64::new(0));
        if let Some(staged) = state.nar_storage.staged() {
            let staged = Arc::clone(staged);
            let wakes = Arc::clone(&wakes);
            let actor = myself.clone();
            let cancel = ctx.cancel.clone();
            state.shutdown.spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = staged.woken() => {
                            wakes.fetch_add(1, Ordering::Relaxed);
                            if actor.cast(UploaderMsg::Wake).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }

        myself.cast(UploaderMsg::Tick)?;
        Ok(UploaderState {
            state,
            ctx,
            backoff: Arc::new(Mutex::new(HashMap::new())),
            wakes,
            wakes_seen: 0,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        st: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let tick = matches!(msg, UploaderMsg::Tick);
        let wakes = st.wakes.load(Ordering::Relaxed);
        if !tick && wakes == st.wakes_seen {
            return Ok(());
        }

        let state = Arc::clone(&st.state);
        let backoff = Arc::clone(&st.backoff);
        let scope = if tick { Scope::Tick } else { Scope::Wake };
        let alive = run_pass(
            HEALTH_NAME,
            UPLOAD_PASS_BUDGET,
            &st.ctx.cancel,
            &st.ctx.health,
            Box::pin(async move {
                let report = run_upload_pass(&state, &backoff, scope)
                    .await
                    .map_err(PassError::from)?;
                if report != PassReport::default() {
                    info!(?report, "nar-uploader pass");
                }

                Ok(())
            }),
        )
        .await;
        st.wakes_seen = wakes;

        if !alive {
            myself.stop(Some("shutdown".into()));
        } else if tick {
            myself.send_after(UPLOAD_TICK, || UploaderMsg::Tick);
        }

        Ok(())
    }
}

/// The uploader as a supervised child of the root, on both backends: a local
/// store never stages, so its passes are one indexed count of zero rows.
pub fn child_spec(state: &Arc<ServerState>) -> ChildSpec {
    let state = Arc::clone(state);
    ChildSpec::Custom {
        stop_last: false,
        name: HEALTH_NAME,
        spawn: Arc::new(move |ctx: ChildCtx| {
            let state = Arc::clone(&state);
            Box::pin(async move {
                let parent = ctx.parent.clone();
                let (actor, _) = Actor::spawn_linked(None, Uploader, (state, ctx), parent).await?;
                Ok(actor.get_cell())
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cacher::test_support::test_server_state;
    use gradient_entity::cached_path::Model as MCachedPath;
    use gradient_storage::{NarStore, StagedNars};
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::BTreeMap;
    use std::path::Path;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccc";
    const D: &str = "dddddddddddddddddddddddddddddddd";

    fn store(base: &Path) -> NarStore {
        NarStore::local(base.to_str().unwrap())
            .unwrap()
            .with_staging(StagedNars::new(base.join("nar-staged")).unwrap())
    }

    fn state(base: &Path, rows: Vec<MCachedPath>) -> Arc<ServerState> {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([rows])
            .into_connection();
        test_server_state(store(base), db, |config| {
            config.storage.nar_upload_grace_hours = 1;
            config.storage.nar_upload_concurrency = 2;
        })
    }

    fn row(hash: &str, size: i64, age: chrono::Duration) -> MCachedPath {
        MCachedPath {
            hash: hash.into(),
            file_hash: Some("sha256:abc".into()),
            file_size: Some(size),
            created_at: gradient_types::now() - age,
            ..Default::default()
        }
    }

    async fn stage(state: &ServerState, hash: &str, bytes: &[u8]) {
        let claim =
            std::path::PathBuf::from(&state.config.storage.base_path).join(format!("claim-{hash}"));
        tokio::fs::write(&claim, bytes).await.unwrap();
        state
            .nar_storage
            .staged()
            .unwrap()
            .adopt(hash, &claim)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_staged_row_is_uploaded_and_its_file_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path(), vec![row(A, 3, chrono::Duration::seconds(1))]);
        stage(&state, A, b"nar").await;
        let backoff = Mutex::new(HashMap::new());

        let report = run_upload_pass(&state, &backoff, Scope::Wake)
            .await
            .unwrap();

        assert_eq!(report.uploaded, 1, "{report:?}");
        assert!(
            state.nar_storage.exists(A).await.unwrap(),
            "the object is in storage"
        );
        assert!(
            !state.nar_storage.staged().unwrap().exists(A).await,
            "the staged file is gone"
        );
    }

    #[tokio::test]
    async fn a_row_whose_object_is_present_is_confirmed_on_a_tick() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path(), vec![row(B, 3, chrono::Duration::seconds(1))]);
        state.nar_storage.put(B, b"xyz".to_vec()).await.unwrap();
        let backoff = Mutex::new(HashMap::new());

        let report = run_upload_pass(&state, &backoff, Scope::Tick)
            .await
            .unwrap();

        assert_eq!(report.confirmed, 1, "{report:?}");
    }

    #[tokio::test]
    async fn an_old_row_with_neither_file_nor_object_is_demoted_and_a_young_one_waits() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(
            tmp.path(),
            vec![
                row(C, 3, chrono::Duration::hours(2)),
                row(D, 3, chrono::Duration::minutes(1)),
            ],
        );
        let backoff = Mutex::new(HashMap::new());

        let report = run_upload_pass(&state, &backoff, Scope::Tick)
            .await
            .unwrap();

        assert_eq!((report.demoted, report.skipped), (1, 1), "{report:?}");
    }

    #[tokio::test]
    async fn a_wake_pass_leaves_absent_rows_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path(), vec![row(C, 3, chrono::Duration::hours(2))]);
        let backoff = Mutex::new(HashMap::new());

        let report = run_upload_pass(&state, &backoff, Scope::Wake)
            .await
            .unwrap();

        assert_eq!(
            report,
            PassReport::default(),
            "absent rows are the tick's job"
        );
    }

    #[tokio::test]
    async fn a_failed_upload_backs_off_and_the_row_stays() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("nars"),
            b"a file where the shard dir must be",
        )
        .unwrap();
        let state = state(tmp.path(), vec![row(A, 3, chrono::Duration::seconds(1))]);
        stage(&state, A, b"nar").await;
        let backoff = Mutex::new(HashMap::new());

        let report = run_upload_pass(&state, &backoff, Scope::Wake)
            .await
            .unwrap();

        assert_eq!(report.failed, 1, "{report:?}");
        let entry = backoff.lock().get(A).copied().expect("a backoff entry");
        assert_eq!(entry.attempts, 1);
        assert!(entry.retry_after > Instant::now());
        assert!(
            state.nar_storage.staged().unwrap().exists(A).await,
            "the file waits for the retry"
        );
    }

    /// The pass reads one page of the table, so a staged file the page does not
    /// name may still be wanted. Deleting on the page alone made a backlog deeper
    /// than [`UPLOAD_SCAN_LIMIT`] delete the files it had yet to upload, and those
    /// rows demoted at their grace with no object anywhere: 18,780 NARs on one
    /// instance, and every dependent build failing on an input nothing could serve.
    #[tokio::test]
    async fn a_staged_file_beyond_the_page_is_kept_because_the_table_still_wants_it() {
        let tmp = tempfile::tempdir().unwrap();
        // The sweep asks the table one question: which candidates it still wants.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "hash".to_owned(),
                sea_orm::Value::from(B),
            )])]])
            .into_connection();
        let state = test_server_state(store(tmp.path()), db, |_| {});
        stage(&state, A, b"an orphan nothing names").await;
        stage(&state, B, b"beyond the page").await;
        let staged = state.nar_storage.staged().unwrap();

        let removed = sweep_orphans(&state, staged, &HashSet::new(), Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(removed, 1, "only the hash the table disowns");
        assert!(
            staged.exists(B).await,
            "the row beyond the page still wants its file"
        );
        assert!(!staged.exists(A).await, "the real orphan is gone");
    }

    #[tokio::test]
    async fn an_orphan_staged_file_is_removed_once_it_is_old_enough() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path(), vec![]);
        stage(&state, A, b"orphan").await;
        let staged = state.nar_storage.staged().unwrap();

        let fresh = orphan_candidates(staged, &HashSet::new(), Duration::from_secs(3600))
            .await
            .unwrap();
        assert!(
            fresh.is_empty(),
            "a fresh file may belong to a commit still in flight"
        );

        let candidates = orphan_candidates(staged, &HashSet::new(), Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(candidates, vec![A.to_owned()]);
        let removed = remove_orphans(staged, &candidates, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(removed, 1);
        assert!(!staged.exists(A).await);
    }

    #[test]
    fn backoff_doubles_from_thirty_seconds_and_caps_at_fifteen_minutes() {
        let now = Instant::now();
        let first = Backoff::next(None, now);
        assert_eq!(
            (first.attempts, first.retry_after - now),
            (1, Duration::from_secs(30))
        );
        let second = Backoff::next(Some(first), now);
        assert_eq!(second.retry_after - now, Duration::from_secs(60));
        let mut b = second;
        for _ in 0..10 {
            b = Backoff::next(Some(b), now);
        }

        assert_eq!(b.retry_after - now, Duration::from_secs(900));
    }
}
