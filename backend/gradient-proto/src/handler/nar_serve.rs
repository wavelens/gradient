/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Duration;

use gradient_db::store_path_content_variant_reader_choice::{
    StorePathContentVariantReader,
    get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache,
};
use gradient_graph::Demotion;
use gradient_storage::passthrough::{
    PassthroughError, PassthroughLimits, PassthroughRequest, send_nar,
};
use gradient_types::{MCachedPath, StorePath, StorePathMapper};
use gradient_util::telemetry::{GAUGES, Gauges};
use gradient_wire::messages::ServerMessage;
use gradient_wire::session::frame::send_server_msg;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::warn;

use super::SessionHandles;
use super::socket::ProtoWriter;

pub(super) struct ServeSlot {
    _connection: OwnedSemaphorePermit,
    _server: OwnedSemaphorePermit,
    gauges: &'static Gauges,
}

struct Waiting(&'static Gauges);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.serves_waiting.dec();
    }
}

impl ServeSlot {
    pub(super) async fn acquire(
        connection: Arc<Semaphore>,
        server: Arc<Semaphore>,
    ) -> Option<Self> {
        Self::acquire_with(connection, server, &GAUGES).await
    }

    /// The connection's own permit comes first. A connection queueing many paths must not hold
    /// server-wide permits while it waits for its own.
    pub(super) async fn acquire_with(
        connection: Arc<Semaphore>,
        server: Arc<Semaphore>,
        gauges: &'static Gauges,
    ) -> Option<Self> {
        gauges.serves_waiting.inc();
        let waiting = Waiting(gauges);
        let connection = connection.acquire_owned().await.ok()?;
        let server = server.acquire_owned().await.ok()?;
        drop(waiting);
        gauges.serves_active.inc();

        Some(Self {
            _connection: connection,
            _server: server,
            gauges,
        })
    }
}

impl Drop for ServeSlot {
    fn drop(&mut self) {
        self.gauges.serves_active.dec();
    }
}

pub(super) struct NarStreamResume<'a> {
    pub(super) received_bytes: u64,
    pub(super) stream_token: &'a str,
}

pub(super) async fn serve_nar_request(
    handles: &SessionHandles,
    writer: &ProtoWriter,
    mapper: &StorePathMapper,
    reader: StorePathContentVariantReader,
    job_id: &str,
    store_path: &str,
    resume: Option<NarStreamResume<'_>>,
) -> anyhow::Result<u64> {
    let nar_cfg = &handles.db.config.nar;
    let limits = PassthroughLimits {
        open: Duration::from_secs(nar_cfg.storage_open_timeout_secs.get()),
        chunk_read: Duration::from_secs(nar_cfg.send_chunk_timeout_secs.get()),
        chunk_bytes: usize::try_from(nar_cfg.chunk_bytes).unwrap_or(usize::MAX),
    };
    let original = mapper
        .outbound_text(store_path)
        .unwrap_or_else(|_| store_path.to_owned());
    let Ok(path) = StorePath::parse_stored(store_path) else {
        let reason = format!("invalid store path: {original}");
        unavailable(writer, job_id, &original, &reason).await;
        return Err(anyhow::anyhow!(reason));
    };
    let selected = get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
        &handles.db.worker_db,
        reader,
        &[path.hash().to_owned()],
    )
    .await
    .map(|mut selected| selected.remove(path.hash()));
    let Ok(Some(content_variant)) = selected else {
        let reason = match selected {
            Err(e) => format!("content variant lookup failed for {original}: {e}"),
            Ok(_) => format!("no trusted content variant stored: {original}"),
        };
        unavailable(writer, job_id, &original, &reason).await;
        return Err(anyhow::anyhow!(reason));
    };
    let req = PassthroughRequest {
        job_id,
        store_path: &original,
        key: &content_variant.nar_object_storage_key,
        resume_from: resume.as_ref().map_or(0, |resume| resume.received_bytes),
        client_token: resume.as_ref().map(|resume| resume.stream_token),
    };
    match send_nar(&handles.db.storage.nar_storage, writer, req, limits).await {
        Ok(bytes) => Ok(bytes),
        Err(PassthroughError::NotFound(reason)) => {
            unavailable(writer, job_id, &original, &reason).await;
            invalidate_content_variant(handles, &content_variant, store_path).await;
            Err(anyhow::anyhow!(reason))
        }
        Err(e) => Err(e.into()),
    }
}

async fn unavailable(writer: &ProtoWriter, job_id: &str, store_path: &str, reason: &str) {
    let msg = ServerMessage::NarUnavailable {
        job_id: job_id.to_owned(),
        store_path: store_path.to_owned(),
        reason: reason.to_owned(),
    };
    let _ = send_server_msg(writer, &msg).await;
}

async fn invalidate_content_variant(
    handles: &SessionHandles,
    content_variant: &MCachedPath,
    store_path: &str,
) {
    let key = &content_variant.nar_object_storage_key;
    handles.db.storage.nar_storage.hot().invalidate(key);
    match handles
        .graph
        .demote(Demotion::MissingNarOfContentVariant {
            content_variant: content_variant.id,
        })
        .await
    {
        Ok(_) => warn!(
            %key,
            %store_path,
            "self-heal: NAR missing from storage; cached_path demoted so the path will be rebuilt"
        ),
        Err(e) => {
            warn!(%key, %store_path, error = %e, "self-heal: failed to demote cached output")
        }
    }
}

#[cfg(test)]
mod serve_nar_tests {
    use super::*;
    use bytes::Bytes;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_test_support::state::TestState;
    use gradient_test_support::state::test_state;
    use gradient_wire::messages::ServerMessage;
    use tokio::sync::mpsc;

    fn identity() -> StorePathMapper {
        StorePathMapper::for_cache(
            Arc::new(gradient_types::IdentityMapping),
            gradient_types::ids::CacheId::nil(),
        )
    }

    fn spy_writer(timeout: Duration) -> (ProtoWriter, mpsc::Receiver<Bytes>) {
        ProtoWriter::spy(timeout)
    }

    fn decode(bytes: Bytes) -> ServerMessage {
        gradient_wire::codec::from_bytes::<ServerMessage>(
            bytes,
            *gradient_wire::PROTO_VERSIONS.end(),
        )
        .expect("deserialise ServerMessage")
    }

    const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    const OBJECT_KEY: &str = "0123456789abcdfghijklmnpqrsvwxyz-variant";

    fn content_variant() -> MCachedPath {
        MCachedPath {
            hash: HASH.into(),
            nar_object_storage_key: OBJECT_KEY.into(),
            file_hash: "sha256:x".into(),
            created_at: gradient_types::now() - chrono::Duration::days(1),
            ..Default::default()
        }
    }

    fn db_selecting_one_content_variant() -> sea_orm::DatabaseConnection {
        mock_db()
            .append_query_results([vec![content_variant()]])
            .into_connection()
    }

    fn any_cache() -> StorePathContentVariantReader {
        StorePathContentVariantReader::Cache(gradient_types::ids::CacheId::nil())
    }

    #[tokio::test]
    async fn serve_streams_full_payload_in_chunks() {
        let state = test_state(db_selecting_one_content_variant());
        let mut payload = Vec::with_capacity(9 * 1024 * 1024);
        for i in 0..(9 * 1024 * 1024 / 4) {
            payload.extend_from_slice(&(i as u32).to_le_bytes());
        }
        let hash = HASH;
        state
            .nar_storage
            .put(OBJECT_KEY, payload.clone())
            .await
            .unwrap();

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(
            &SessionHandles::from_state(&state),
            &writer,
            &identity(),
            any_cache(),
            "job-1",
            &store_path,
            None,
        )
        .await
        .expect("serve must succeed");

        let mut assembled = Vec::with_capacity(payload.len());
        let mut nar_push_frames = 0u32;
        let mut saw_header = false;
        let mut saw_final = false;
        while let Ok(bytes) = rx.try_recv() {
            match decode(bytes) {
                ServerMessage::NarStreamHeader { total_bytes, .. } => {
                    saw_header = true;
                    assert!(!saw_final, "header must precede chunks");
                    assert_eq!(
                        usize::try_from(total_bytes).expect("total fits"),
                        payload.len()
                    );
                }
                ServerMessage::NarPush { data, is_final, .. } => {
                    assembled.extend_from_slice(&data);
                    if is_final {
                        saw_final = true;
                    }
                    nar_push_frames += 1;
                }
                other => panic!("unexpected frame: {}", other.variant_name()),
            }
        }
        assert!(saw_header, "a NarStreamHeader must precede the chunks");
        assert!(
            nar_push_frames >= 3,
            "9 MiB in 512 KiB chunks is at least 3 frames, got {nar_push_frames}"
        );
        assert!(saw_final, "the last frame must be is_final=true");
        assert_eq!(
            assembled, payload,
            "concatenated NarPush data must equal source"
        );
    }

    #[tokio::test]
    async fn serve_emits_nar_unavailable_when_missing() {
        let db = empty_db();
        let state = test_state(db);
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &SessionHandles::from_state(&state),
            &writer,
            &identity(),
            any_cache(),
            "job-1",
            "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-missing",
            None,
        )
        .await;
        assert!(res.is_err(), "missing path must surface as Err");

        let bytes = rx.try_recv().expect("expect one frame");
        let msg = decode(bytes);
        assert_eq!(msg.variant_name(), "NarUnavailable");
        assert!(
            rx.try_recv().is_err(),
            "no further frames after NarUnavailable"
        );
    }

    #[tokio::test]
    async fn serve_answers_a_hot_entry_in_bulk_chunks() {
        let cli = gradient_test_support::prelude::test_cli();
        let storage = gradient_storage::NarStore::local(&cli.server.base_dir)
            .unwrap()
            .with_hot_cache(gradient_storage::HotNarCache::new(
                4 * 1024 * 1024,
                2 * 1024 * 1024,
            ));
        let state = TestState::new()
            .worker_db(db_selecting_one_content_variant())
            .nar_storage(storage)
            .build();
        let payload: Vec<u8> = (0..=u8::MAX).cycle().take(1024 * 1024 + 123).collect();
        let hash = HASH;
        state
            .nar_storage
            .hot()
            .insert(OBJECT_KEY, Bytes::from(payload.clone()));

        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let store_path = format!("/nix/store/{hash}-test-pkg");
        serve_nar_request(
            &SessionHandles::from_state(&state),
            &writer,
            &identity(),
            any_cache(),
            "job-1",
            &store_path,
            None,
        )
        .await
        .expect("serve from RAM");

        let mut assembled = Vec::new();
        let mut frames = 0u32;
        while let Ok(bytes) = rx.try_recv() {
            match decode(bytes) {
                ServerMessage::NarStreamHeader { total_bytes, .. } => {
                    assert_eq!(
                        usize::try_from(total_bytes).expect("total fits"),
                        payload.len()
                    );
                }
                ServerMessage::NarPush { data, .. } => {
                    assembled.extend_from_slice(&data);
                    frames += 1;
                }
                other => panic!("unexpected frame: {}", other.variant_name()),
            }
        }
        assert_eq!(assembled, payload);
        assert!(
            frames >= 3,
            "1 MiB in 512 KiB chunks is at least three frames"
        );
        assert_eq!(state.nar_storage.hot().stats().hits, 1);
    }

    #[tokio::test]
    async fn an_invalid_store_path_is_unavailable() {
        let db = empty_db();
        let state = test_state(db);
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &SessionHandles::from_state(&state),
            &writer,
            &identity(),
            any_cache(),
            "job-1",
            "not_a_store_path",
            None,
        )
        .await;

        assert!(res.is_err());
        let msg = decode(rx.try_recv().expect("one frame"));
        assert_eq!(msg.variant_name(), "NarUnavailable");
        assert!(rx.try_recv().is_err());
    }

    fn assembled_nar(rx: &mut mpsc::Receiver<Bytes>) -> Vec<u8> {
        let mut assembled = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            if let ServerMessage::NarPush { data, .. } = decode(bytes) {
                assembled.extend_from_slice(&data);
            }
        }
        assembled
    }

    #[tokio::test]
    async fn each_reader_receives_the_bytes_of_the_content_variant_it_trusts() {
        use gradient_fixtures::store_path_content_variants::{
            FIRST_KEY, SECOND_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let state = TestState::new().worker_db(readers.db.connection()).build();
        state
            .nar_storage
            .put(FIRST_KEY, b"variant of project A".to_vec())
            .await
            .unwrap();
        state
            .nar_storage
            .put(SECOND_KEY, b"variant of project B".to_vec())
            .await
            .unwrap();
        let store_path = format!("/nix/store/{SHARED}-shared");

        let mut received = Vec::new();
        for project in [readers.project_a, readers.project_b] {
            let (writer, mut rx) = spy_writer(Duration::from_secs(5));
            serve_nar_request(
                &SessionHandles::from_state(&state),
                &writer,
                &identity(),
                StorePathContentVariantReader::Project(project),
                "job-1",
                &store_path,
                None,
            )
            .await
            .expect("serve the trusted variant");
            received.push(assembled_nar(&mut rx));
        }

        assert_eq!(
            received,
            vec![
                b"variant of project A".to_vec(),
                b"variant of project B".to_vec()
            ]
        );
        readers.db.finish().await;
    }

    struct WorkerAnswersOnSharedBuild {
        requested: Option<Vec<u8>>,
        resumed: Option<Vec<u8>>,
        pulled_nar_hashes: Vec<Option<String>>,
    }

    async fn answers_to_the_build_job_of_project_a_on_the_shared_build_of_project_b(
        readers: &gradient_fixtures::store_path_content_variants::Readers,
    ) -> WorkerAnswersOnSharedBuild {
        use super::super::cache::{CacheQueryJob, handle_cache_query};
        use super::super::inbound::{ActiveJob, ActiveJobs, fixture::pending_build};
        use gradient_fixtures::store_path_content_variants::{FIRST_KEY, SECOND_KEY, SHARED};
        use gradient_scheduler::jobs::PendingJob;

        let state = TestState::new()
            .worker_db(readers.db.connection())
            .web_db(readers.db.connection())
            .cache_db(readers.db.connection())
            .build();
        for (key, bytes) in [
            (FIRST_KEY, b"variant of project A"),
            (SECOND_KEY, b"variant of project B"),
        ] {
            state.nar_storage.put(key, bytes.to_vec()).await.unwrap();
        }
        let handles = SessionHandles::from_state(&state);
        let PendingJob::Build(mut build_job) = pending_build(readers.build_b) else {
            unreachable!("build jobs come from the fixture");
        };
        build_job.project_id = readers.project_a;
        let active = ActiveJobs::from(std::collections::HashMap::from([(
            "j1".to_owned(),
            ActiveJob {
                assignment_id: gradient_types::ids::DispatchedJobId::now_v7(),
                pending: PendingJob::Build(build_job),
                cluster: None,
                input_closure_set: Default::default(),
            },
        )]));
        let reader = active
            .store_path_content_variant_reader_for_job("j1")
            .expect("the build job is active");
        let store_path = format!("/nix/store/{SHARED}-shared");

        let mut served = Vec::new();
        for resume in [
            None,
            Some(NarStreamResume {
                received_bytes: 0,
                stream_token: "token-of-another-stream",
            }),
        ] {
            let (writer, mut rx) = spy_writer(Duration::from_secs(5));
            let result = serve_nar_request(
                &handles,
                &writer,
                &identity(),
                reader.clone(),
                "j1",
                &store_path,
                resume,
            )
            .await;
            served.push(result.ok().map(|_| assembled_nar(&mut rx)));
        }
        let pulled = handle_cache_query(
            &handles,
            "w1",
            CacheQueryJob {
                project: readers.project_a,
                content_variant_reader: reader,
            },
            std::slice::from_ref(&store_path),
            &[],
            gradient_wire::types::QueryMode::Pull,
            false,
        )
        .await
        .expect("cache query ran");
        let resumed = served.pop().flatten();
        WorkerAnswersOnSharedBuild {
            requested: served.pop().flatten(),
            resumed,
            pulled_nar_hashes: pulled.into_iter().map(|entry| entry.nar_hash).collect(),
        }
    }

    #[tokio::test]
    async fn test_build_job_gets_stored_nar_variant_trusted_by_every_project_with_live_job_on_shared_derivation_build()
     {
        let readers = gradient_fixtures::store_path_content_variants::create_test_database_with_project_a_build_job_on_project_b_derivation_and_optional_upload_cache_read(true).await;

        let answers =
            answers_to_the_build_job_of_project_a_on_the_shared_build_of_project_b(&readers).await;

        let variant_of_b = Some(b"variant of project B".to_vec());
        assert_eq!(
            (
                answers.requested,
                answers.resumed,
                answers.pulled_nar_hashes
            ),
            (
                variant_of_b.clone(),
                variant_of_b,
                vec![Some("sha256:second".to_owned())]
            )
        );
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn build_job_on_shared_derivation_build_gets_no_nar_variant_when_no_variant_is_trusted_by_every_project_with_a_live_job()
     {
        let readers = gradient_fixtures::store_path_content_variants::create_test_database_with_project_a_build_job_on_project_b_derivation_and_optional_upload_cache_read(false).await;

        let answers =
            answers_to_the_build_job_of_project_a_on_the_shared_build_of_project_b(&readers).await;

        assert_eq!(
            (
                answers.requested,
                answers.resumed,
                answers.pulled_nar_hashes
            ),
            (None, None, vec![])
        );
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn build_job_gets_no_nar_variant_when_all_jobs_on_shared_derivation_build_are_aborted() {
        let readers = gradient_fixtures::store_path_content_variants::create_test_database_with_project_a_build_job_on_project_b_derivation_and_optional_upload_cache_read(true).await;
        readers
            .db
            .execute("UPDATE build_job SET aborted = true")
            .await;

        let answers =
            answers_to_the_build_job_of_project_a_on_the_shared_build_of_project_b(&readers).await;

        assert_eq!(
            (
                answers.requested,
                answers.resumed,
                answers.pulled_nar_hashes
            ),
            (None, None, vec![])
        );
        readers.db.finish().await;
    }

    const TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH: &str = "88888888888888888888888888888888";
    const UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH: &str =
        "99999999999999999999999999999999";
    const UNSIGNED_STORED_COPY_STORE_PATH_HASH_NOT_IN_INPUT_CLOSURE: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    struct ProjectWithoutSigningKeyLiveBuildOnSharedBuildFixture {
        db: gradient_fixtures::database::TestDatabase,
        handles: SessionHandles,
        project_without_signing_cache_id_with_job_on_shared_build: gradient_types::ids::ProjectId,
        shared_build_id: gradient_types::ids::DerivationBuildId,
    }

    async fn build_input_closure_reachability_fixture()
    -> ProjectWithoutSigningKeyLiveBuildOnSharedBuildFixture {
        use gradient_types::ids::EvaluationId;

        let (dependency_evaluation_id, parent_derivation_evaluation_id) =
            (EvaluationId::now_v7(), EvaluationId::now_v7());
        let dependency_derivation_store_path =
            format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent_derivation_store_path = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(dependency_evaluation_id)
            .derivation(&dependency_derivation_store_path)
            .output(TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .cached_path(TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH, "dependency")
            .references(&[(
                UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH,
                "reference",
            )])
            .cached_path(
                UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH,
                "reference",
            )
            .cached_path(
                UNSIGNED_STORED_COPY_STORE_PATH_HASH_NOT_IN_INPUT_CLOSURE,
                "outside",
            )
            .cached_path(
                UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST,
                "late",
            )
            .evaluation(parent_derivation_evaluation_id)
            .derivation(&parent_derivation_store_path)
            .depends_on(&dependency_derivation_store_path)
            .job()
            .into_database()
            .await;
        let state = TestState::new()
            .worker_db(db.connection())
            .web_db(db.connection())
            .cache_db(db.connection())
            .build();
        for hash in [
            TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH,
            UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH,
            UNSIGNED_STORED_COPY_STORE_PATH_HASH_NOT_IN_INPUT_CLOSURE,
            UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST,
        ] {
            state
                .nar_storage
                .put(
                    &format!("{hash}-abc"),
                    format!("nar of {hash}").into_bytes(),
                )
                .await
                .unwrap();
        }
        ProjectWithoutSigningKeyLiveBuildOnSharedBuildFixture {
            handles: SessionHandles::from_state(&state),
            project_without_signing_cache_id_with_job_on_shared_build: db
                .project_of(parent_derivation_evaluation_id)
                .await,
            shared_build_id: db.shared_build(&parent_derivation_store_path).await.id,
            db,
        }
    }

    fn active_jobs_map_with_shared_build_job_under_job_id(
        fixture: &ProjectWithoutSigningKeyLiveBuildOnSharedBuildFixture,
        job_ids: &[&str],
    ) -> super::super::inbound::ActiveJobs {
        use super::super::inbound::{ActiveJob, ActiveJobs, fixture::pending_build};
        use gradient_scheduler::jobs::PendingJob;

        ActiveJobs::from(
            job_ids
                .iter()
                .map(|job_id| {
                    let PendingJob::Build(mut build_job) = pending_build(fixture.shared_build_id)
                    else {
                        unreachable!("build jobs come from the fixture");
                    };
                    build_job.project_id =
                        fixture.project_without_signing_cache_id_with_job_on_shared_build;
                    (
                        (*job_id).to_owned(),
                        ActiveJob {
                            assignment_id: gradient_types::ids::DispatchedJobId::now_v7(),
                            pending: PendingJob::Build(build_job),
                            cluster: None,
                            input_closure_set: Default::default(),
                        },
                    )
                })
                .collect::<std::collections::HashMap<_, _>>(),
        )
    }

    async fn request_nar_bytes_for_one_store_path_hash(
        fixture: &ProjectWithoutSigningKeyLiveBuildOnSharedBuildFixture,
        reader: StorePathContentVariantReader,
        hash: &str,
    ) -> Result<Vec<u8>, String> {
        let store_path = format!("/nix/store/{hash}-path");
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));
        let result = serve_nar_request(
            &fixture.handles,
            &writer,
            &identity(),
            reader,
            "j1",
            &store_path,
            None,
        )
        .await;
        match result {
            Ok(_) => Ok(assembled_nar(&mut rx)),
            Err(_) => match decode(rx.try_recv().expect("one frame")) {
                ServerMessage::NarUnavailable { reason, .. } => {
                    Err(reason.replacen(&store_path, "", 1))
                }
                other => panic!("unexpected frame: {}", other.variant_name()),
            },
        }
    }

    async fn send_pull_path_info_cache_query_and_return_sorted_answered_store_paths(
        fixture: &ProjectWithoutSigningKeyLiveBuildOnSharedBuildFixture,
        reader: StorePathContentVariantReader,
        hashes: &[&str],
    ) -> Vec<String> {
        use super::super::cache::{CacheQueryJob, handle_cache_query};

        let requested_store_paths: Vec<String> = hashes
            .iter()
            .map(|hash| format!("/nix/store/{hash}-path"))
            .collect();
        let mut sorted_store_paths_answered_by_path_info_cache_query: Vec<String> =
            handle_cache_query(
                &fixture.handles,
                "w1",
                CacheQueryJob {
                    project: fixture.project_without_signing_cache_id_with_job_on_shared_build,
                    content_variant_reader: reader,
                },
                &requested_store_paths,
                &[],
                gradient_wire::types::QueryMode::Pull,
                false,
            )
            .await
            .expect("cache query ran")
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        sorted_store_paths_answered_by_path_info_cache_query.sort();
        sorted_store_paths_answered_by_path_info_cache_query
    }

    #[tokio::test]
    async fn build_job_without_signing_cache_serves_direct_input_and_reference_and_refuses_path_outside_input_closure()
     {
        let test_database_sessions_and_shared_build_id_for_live_build_job =
            build_input_closure_reachability_fixture().await;
        let running_jobs_by_job_id = active_jobs_map_with_shared_build_job_under_job_id(
            &test_database_sessions_and_shared_build_id_for_live_build_job,
            &["j1"],
        );
        let content_variant_reader_for_job_j1 = running_jobs_by_job_id
            .store_path_content_variant_reader_for_job("j1")
            .expect("the build job is active");
        let requested_store_path_hashes = [
            TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH,
            UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH,
            UNSIGNED_STORED_COPY_STORE_PATH_HASH_NOT_IN_INPUT_CLOSURE,
        ];

        let sorted_store_paths_answered_by_path_info_cache_query =
            send_pull_path_info_cache_query_and_return_sorted_answered_store_paths(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_job_j1.clone(),
                &requested_store_path_hashes,
            )
            .await;
        let mut nar_bytes_or_refusal_by_requested_hash = Vec::new();
        for hash in requested_store_path_hashes {
            nar_bytes_or_refusal_by_requested_hash.push(
                request_nar_bytes_for_one_store_path_hash(
                    &test_database_sessions_and_shared_build_id_for_live_build_job,
                    content_variant_reader_for_job_j1.clone(),
                    hash,
                )
                .await,
            );
        }
        let expected_nar_unavailable_for_store_path_without_stored_copy =
            request_nar_bytes_for_one_store_path_hash(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_job_j1,
                &"c".repeat(32),
            )
            .await;
        test_database_sessions_and_shared_build_id_for_live_build_job
            .db
            .finish()
            .await;

        assert_eq!(
            sorted_store_paths_answered_by_path_info_cache_query,
            vec![
                format!("/nix/store/{TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH}-path"),
                format!("/nix/store/{UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH}-path"),
            ],
            "Path info can respond to direct inputs and their references for build jobs of projects without a signing cache. Paths outside the input closure of a shared build get no response."
        );
        assert!(expected_nar_unavailable_for_store_path_without_stored_copy.is_err());
        assert_eq!(
            nar_bytes_or_refusal_by_requested_hash,
            vec![
                Ok(format!("nar of {TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH}").into_bytes()),
                Ok(
                    format!("nar of {UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH}")
                        .into_bytes()
                ),
                expected_nar_unavailable_for_store_path_without_stored_copy,
            ],
            "NAR requests serve the bytes of the direct inputs and their references. The server can reject a path outside the input closure with the same response as a path with no stored copy."
        );
    }

    #[tokio::test]
    async fn exec_job_gets_program_closure_nar_from_cache_project_does_not_read_and_refuses_path_outside_program_closure_same_as_no_stored_copy()
     {
        use super::super::inbound::{ActiveJob, ActiveJobs, fixture::pending_exec};
        use gradient_scheduler::jobs::PendingJob;
        use gradient_types::ids::{CacheId, EvaluationId, ExecJobAttemptId};

        let program_output = "d".repeat(32);
        let dependency_output = "f".repeat(32);
        let outside_closure = "g".repeat(32);
        let dependency_derivation = format!("/nix/store/{}-dependency.drv", "h".repeat(32));
        let (program_evaluation, uploader_evaluation) =
            (EvaluationId::now_v7(), EvaluationId::now_v7());
        let unread_cache = CacheId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(program_evaluation)
            .derivation(&dependency_derivation)
            .output(&dependency_output)
            .derivation(&format!("/nix/store/{}-program.drv", "j".repeat(32)))
            .output(&program_output)
            .runtime_depends_on(&dependency_derivation)
            .evaluation(uploader_evaluation)
            .into_cache(unread_cache)
            .cached_path(&dependency_output, "dependency")
            .cached_path(&outside_closure, "outside")
            .into_database()
            .await;
        db.execute("DELETE FROM store_path_content_producer").await;
        db.execute(&format!(
            "INSERT INTO cached_path_signature (id, cached_path, cache, created_at) \
             SELECT uuidv7(), c.id, '{unread_cache}', c.created_at FROM cached_path c"
        ))
        .await;
        let state = TestState::new()
            .worker_db(db.connection())
            .web_db(db.connection())
            .cache_db(db.connection())
            .build();
        for hash in [&dependency_output, &outside_closure] {
            state
                .nar_storage
                .put(
                    &format!("{hash}-abc"),
                    format!("nar of {hash}").into_bytes(),
                )
                .await
                .unwrap();
        }
        let PendingJob::Exec(mut exec_job) = pending_exec(ExecJobAttemptId::now_v7()) else {
            unreachable!("exec jobs come from the fixture");
        };
        exec_job.project_id = db.project_of(program_evaluation).await;
        exec_job.spec.program = format!("/nix/store/{program_output}-program/bin/program");
        let fixture = ProjectWithoutSigningKeyLiveBuildOnSharedBuildFixture {
            handles: SessionHandles::from_state(&state),
            project_without_signing_cache_id_with_job_on_shared_build: exec_job.project_id,
            shared_build_id: gradient_types::ids::DerivationBuildId::now_v7(),
            db,
        };
        let running_jobs_by_job_id = ActiveJobs::from(std::collections::HashMap::from([(
            "j1".to_owned(),
            ActiveJob {
                assignment_id: gradient_types::ids::DispatchedJobId::now_v7(),
                pending: PendingJob::Exec(exec_job),
                cluster: None,
                input_closure_set: Default::default(),
            },
        )]));
        let reader = running_jobs_by_job_id
            .store_path_content_variant_reader_for_job("j1")
            .expect("the exec job is active");

        let dependency_nar =
            request_nar_bytes_for_one_store_path_hash(&fixture, reader.clone(), &dependency_output)
                .await;
        let outside_nar =
            request_nar_bytes_for_one_store_path_hash(&fixture, reader.clone(), &outside_closure)
                .await;
        let no_stored_copy =
            request_nar_bytes_for_one_store_path_hash(&fixture, reader, &"c".repeat(32)).await;
        fixture.db.finish().await;

        assert_eq!(
            dependency_nar,
            Ok(format!("nar of {dependency_output}").into_bytes()),
            "Exec jobs read the paths in the program closure from a cache that the project doesn't read. That cache is the only place where those paths exist."
        );
        assert!(no_stored_copy.is_err());
        assert_eq!(
            outside_nar, no_stored_copy,
            "An exec job is refused a stored path outside its program closure with the same answer as a path without a stored copy."
        );
    }

    #[tokio::test]
    async fn same_job_requests_share_input_closure_set_computed_once_while_second_job_gets_own_set_without_removed_database_reference()
     {
        let test_database_sessions_and_shared_build_id_for_live_build_job =
            build_input_closure_reachability_fixture().await;
        let running_jobs_by_job_id = active_jobs_map_with_shared_build_job_under_job_id(
            &test_database_sessions_and_shared_build_id_for_live_build_job,
            &["j1", "j2"],
        );
        let content_variant_reader_for_active_job_id = |job_id: &str| {
            running_jobs_by_job_id
                .store_path_content_variant_reader_for_job(job_id)
                .expect("the build job is active")
        };

        let job1_first_query_paths_before_reference_removal =
            send_pull_path_info_cache_query_and_return_sorted_answered_store_paths(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_active_job_id("j1"),
                &[UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH],
            )
            .await;
        test_database_sessions_and_shared_build_id_for_live_build_job
            .db
            .execute(&format!(
                "UPDATE cached_path SET \"references\" = NULL \
                 WHERE hash = '{TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH}'"
            ))
            .await;
        let same_job_nar_response_after_reference_removed_from_database =
            request_nar_bytes_for_one_store_path_hash(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_active_job_id("j1"),
                UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH,
            )
            .await;
        let removed_reference_request_result_for_second_job =
            request_nar_bytes_for_one_store_path_hash(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_active_job_id("j2"),
                UNSIGNED_STORED_COPY_LISTED_REFERENCE_STORE_PATH_HASH,
            )
            .await;
        test_database_sessions_and_shared_build_id_for_live_build_job
            .db
            .finish()
            .await;

        assert_eq!(
            (
                job1_first_query_paths_before_reference_removal.len(),
                same_job_nar_response_after_reference_removed_from_database.is_ok(),
                removed_reference_request_result_for_second_job.is_ok()
            ),
            (1, true, false),
            "One job request can use one input closure set. The worker can compute that set once. The database can remove the reference to the closure. The same job can still get that set. A second job on the same shared build can compute its own set. That job can get a rejection."
        );
    }

    #[tokio::test]
    async fn test_store_path_added_to_direct_input_after_first_request_is_served_by_path_info_and_nar_after_recompute_on_miss()
     {
        let test_database_sessions_and_shared_build_id_for_live_build_job =
            build_input_closure_reachability_fixture().await;
        let running_jobs_by_job_id = active_jobs_map_with_shared_build_job_under_job_id(
            &test_database_sessions_and_shared_build_id_for_live_build_job,
            &["j1"],
        );
        let content_variant_reader_for_job_j1 = running_jobs_by_job_id
            .store_path_content_variant_reader_for_job("j1")
            .expect("the build job is active");

        let store_paths_served_in_first_path_info_query =
            send_pull_path_info_cache_query_and_return_sorted_answered_store_paths(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_job_j1.clone(),
                &[TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH],
            )
            .await;
        test_database_sessions_and_shared_build_id_for_live_build_job
            .db
            .execute(&format!(
                "UPDATE cached_path SET \"references\" = \"references\" || \
                 ' {UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST}-late' \
                 WHERE hash = '{TEST_DIRECT_INPUT_UNSIGNED_STORE_PATH_HASH}'"
            ))
            .await;
        let later_referenced_store_paths =
            send_pull_path_info_cache_query_and_return_sorted_answered_store_paths(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_job_j1.clone(),
                &[UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST],
            )
            .await;
        let nar_response_for_store_path_referenced_after_first_request =
            request_nar_bytes_for_one_store_path_hash(
                &test_database_sessions_and_shared_build_id_for_live_build_job,
                content_variant_reader_for_job_j1,
                UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST,
            )
            .await;
        test_database_sessions_and_shared_build_id_for_live_build_job
            .db
            .finish()
            .await;

        assert_eq!(
            (
                store_paths_served_in_first_path_info_query.len(),
                later_referenced_store_paths,
                nar_response_for_store_path_referenced_after_first_request
            ),
            (
                1,
                vec![format!(
                    "/nix/store/{UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST}-path"
                )],
                Ok(format!(
                    "nar of {UNSIGNED_COPY_STORE_PATH_HASH_REFERENCED_AFTER_FIRST_REQUEST}"
                )
                .into_bytes()),
            ),
            "The first stored copy that a direct input can reference after the first request of the job is a miss. A recompute of the set follows and path info and NAR requests provide that copy."
        );
    }

    #[tokio::test]
    async fn a_hash_with_only_an_untrusted_content_variant_is_unavailable_and_demotes_nothing() {
        use gradient_fixtures::store_path_content_variants::{
            UNTRUSTED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let graph = gradient_graph::Graph::new();
        let state = TestState::new()
            .worker_db(readers.db.connection())
            .web_db(readers.db.connection())
            .cache_db(readers.db.connection())
            .graph(Arc::clone(&graph))
            .build();
        graph.spawn(state.db(), None, None).await.unwrap();
        let (writer, mut rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &SessionHandles::from_state(&state),
            &writer,
            &identity(),
            StorePathContentVariantReader::Project(readers.project_b),
            "job-1",
            &format!("/nix/store/{UNTRUSTED}-untrusted"),
            None,
        )
        .await;

        assert!(res.is_err());
        assert_eq!(
            decode(rx.try_recv().expect("one frame")).variant_name(),
            "NarUnavailable"
        );
        assert!(readers.db.cached_path(UNTRUSTED).await.is_some());
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn a_missing_object_retires_only_the_content_variant_the_reader_selected() {
        use gradient_fixtures::store_path_content_variants::{
            FIRST_KEY, SECOND_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let graph = gradient_graph::Graph::new();
        let state = TestState::new()
            .worker_db(readers.db.connection())
            .web_db(readers.db.connection())
            .cache_db(readers.db.connection())
            .graph(Arc::clone(&graph))
            .build();
        graph.spawn(state.db(), None, None).await.unwrap();
        state
            .nar_storage
            .put(SECOND_KEY, b"variant of project B".to_vec())
            .await
            .unwrap();
        let (writer, _rx) = spy_writer(Duration::from_secs(5));

        let res = serve_nar_request(
            &SessionHandles::from_state(&state),
            &writer,
            &identity(),
            StorePathContentVariantReader::Project(readers.project_a),
            "job-1",
            &format!("/nix/store/{SHARED}-shared"),
            None,
        )
        .await;

        assert!(res.is_err());
        let remaining: Vec<String> = readers
            .db
            .stored_contents_for_store_path_hash_oldest_first(SHARED)
            .await
            .into_iter()
            .map(|row| row.nar_object_storage_key)
            .collect();
        assert_eq!(
            remaining,
            vec![SECOND_KEY.to_owned()],
            "{FIRST_KEY} retired"
        );
        readers.db.finish().await;
    }
}

#[cfg(test)]
mod serve_slot_tests {
    use super::*;
    use gradient_util::telemetry::Gauges;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Semaphore;

    fn gauges() -> &'static Gauges {
        Box::leak(Box::new(Gauges::new()))
    }

    fn permits(n: usize) -> Arc<Semaphore> {
        Arc::new(Semaphore::new(n))
    }

    async fn take(
        connection: &Arc<Semaphore>,
        server: &Arc<Semaphore>,
        g: &'static Gauges,
    ) -> Option<ServeSlot> {
        ServeSlot::acquire_with(Arc::clone(connection), Arc::clone(server), g).await
    }

    #[tokio::test]
    async fn a_slot_is_active_until_dropped() {
        let g = gauges();
        let slot = take(&permits(1), &permits(1), g).await.expect("slot");

        assert_eq!(g.serves_active.get(), 1);
        assert_eq!(g.serves_waiting.get(), 0);
        drop(slot);
        assert_eq!(g.serves_active.get(), 0);
    }

    #[tokio::test]
    async fn a_queued_serve_counts_as_waiting() {
        let g = gauges();
        let (sem, server) = (permits(1), permits(8));
        let held = take(&sem, &server, g).await.expect("slot");
        let mut queued = Box::pin(take(&sem, &server, g));
        let still_queued = tokio::time::timeout(Duration::from_millis(20), &mut queued).await;

        assert!(still_queued.is_err());
        assert_eq!(g.serves_waiting.get(), 1);
        drop(held);
        let slot = queued.await.expect("slot");
        assert_eq!(g.serves_waiting.get(), 0);
        assert_eq!(g.serves_active.get(), 1);
        drop(slot);
    }

    #[tokio::test]
    async fn the_server_wide_limit_holds_back_another_connection() {
        let g = gauges();
        let server = permits(1);
        let held = take(&permits(8), &server, g).await.expect("slot");
        let other_connection = permits(8);
        let mut other = Box::pin(take(&other_connection, &server, g));

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut other)
                .await
                .is_err()
        );
        drop(held);
        assert!(other.await.is_some());
    }

    #[tokio::test]
    async fn cancelled_wait_releases_waiting() {
        let g = gauges();
        let (sem, server) = (permits(0), permits(1));
        let waiting = take(&sem, &server, g);
        let cancelled = tokio::time::timeout(Duration::from_millis(20), waiting).await;

        assert!(cancelled.is_err());
        assert_eq!(g.serves_waiting.get(), 0);
        assert_eq!(g.serves_active.get(), 0);
    }

    #[tokio::test]
    async fn a_closed_semaphore_yields_no_slot() {
        let g = gauges();
        let sem = permits(0);
        sem.close();

        assert!(take(&sem, &permits(1), g).await.is_none());
        assert_eq!(g.serves_waiting.get(), 0);
    }
}
