/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::Session;
use crate::store_paths::StoredPathInfo;
use async_compression::Level;
use async_compression::tokio::write::ZstdEncoder;
use gradient_entity::cached_path::join_sorted_deduplicated_references_with_spaces;
use gradient_entity::store_path_content_producer::CacheUploadChannel;
use gradient_graph::{NarCommit, NarCommitContent};
use gradient_proto::import::{
    ImportInput, StoredContentProducer, find_verified_cached_path_by_store_hash, import_nar_reader,
};
use gradient_storage::admission::{HeldPermit, ObjectKey, UploadAdmission};
use gradient_storage::{
    ClientClaimedNarUploadHashesAndSizes, ServerComputedNarAndCompressedFileHashesAndSizes,
    UploadedStorePathAndContentAddress, verify_zstd_nar_stream_sha256_and_sizes_against_claims,
};
use gradient_types::events::cache::NarSigned;
use gradient_types::permissions::Permission;
use gradient_types::*;
use harmonia_store_path_info::NarHash;
use harmonia_utils_hash::{Algorithm, Context, HashFormat as _, Sha256};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub enum Pending {
    Known(MCachedPath),
    New(Staged),
}

pub struct Staged {
    file: tempfile::NamedTempFile,
    verified: ServerComputedNarAndCompressedFileHashesAndSizes,
    permit: HeldPermit,
}

pub async fn import(
    session: &Session,
    info: &StoredPathInfo,
    reader: impl AsyncRead + Unpin + Send,
) -> anyhow::Result<()> {
    let pending = stage(session, info, reader).await?;
    commit(session, info, pending).await
}

pub async fn stage(
    session: &Session,
    info: &StoredPathInfo,
    reader: impl AsyncRead + Unpin + Send,
) -> anyhow::Result<Pending> {
    anyhow::ensure!(
        session.may(Permission::TriggerEvaluation),
        "copying into {} needs TriggerEvaluation",
        session.project.name
    );
    if let Some(reason) = session_upload_refusal_reason(session, info.nar_size).await? {
        anyhow::bail!("{reason}");
    }

    if let Some(known) =
        find_verified_cached_path_with_existing_nar_for_upload(session, info).await?
    {
        return check_uploaded_nar_matches_stored_path(reader, info, known).await;
    }

    let handles = &session.handles;
    let claim = claim_path(
        &handles.db.config,
        &handles.upload_admission,
        info.path.hash(),
        info.nar_size,
    )
    .await?;
    // Another upload of this path can commit it during this upload's wait for its turn.
    if let Some(known) =
        find_verified_cached_path_with_existing_nar_for_upload(session, info).await?
    {
        drop(claim);
        return check_uploaded_nar_matches_stored_path(reader, info, known).await;
    }

    let announced = info.nar_hash.as_sri().to_string();
    let dir = handles.db.config.server.nar_upload_partial_dir();
    tokio::fs::create_dir_all(&dir).await?;
    let file = tempfile::NamedTempFile::new_in(&dir)?;
    let (nar_hash, nar_size, file_hash, file_size) =
        compress_into(reader, file.path(), info.nar_size).await?;
    anyhow::ensure!(
        nar_size == info.nar_size,
        "NAR size of {} does not match",
        info.original
    );
    anyhow::ensure!(
        nar_hash == info.nar_hash,
        "NAR hash of {} does not match",
        info.original
    );

    let store_path_text_from_ssh_client = info.original.to_string();
    let verified = verify_zstd_nar_stream_sha256_and_sizes_against_claims(
        tokio::fs::File::open(file.path()).await?,
        &ClientClaimedNarUploadHashesAndSizes {
            file_hash: &file_hash,
            file_size,
            nar_hash: &announced,
            nar_size,
            claimed_store_path_references: &info.claimed_store_path_references,
            uploaded_nar_with_stated_content_address_to_check: Some(
                UploadedStorePathAndContentAddress {
                    store_path: &store_path_text_from_ssh_client,
                    content_address: info.stated.parsed_content_address(),
                },
            ),
        },
    )
    .await?;

    Ok(Pending::New(Staged {
        file,
        verified,
        permit: claim,
    }))
}

async fn session_upload_refusal_reason(
    session: &Session,
    nar_size: u64,
) -> anyhow::Result<Option<String>> {
    let db = &session.handles.db;
    Ok(session
        .upload_storage
        .lock()
        .await
        .admit_upload_to_project_writable_caches(
            &db.web_db,
            session.project.id,
            db.config.cache.max_storage_gb,
            nar_size,
        )
        .await?)
}

pub async fn commit(
    session: &Session,
    info: &StoredPathInfo,
    pending: Pending,
) -> anyhow::Result<()> {
    let handles = &session.handles;
    let signed = match pending {
        Pending::Known(cached_path) => {
            let producer = get_ssh_nar_upload_stored_content_producer(session);
            handles
                .graph
                .commit_nar(NarCommit {
                    store_path: info.path.full(),
                    content: NarCommitContent::ExistingServerVerifiedCachedPathId(cached_path.id),
                    references: info.stated.references().to_vec(),
                    deriver: info.stated.deriver().map(str::to_owned),
                    ca: info
                        .stated
                        .checked_content_address_text()
                        .map(str::to_owned),
                    producer,
                })
                .await?
                .signed
        }
        Pending::New(staged) => {
            let producer = get_ssh_nar_upload_stored_content_producer(session);
            let store_path = info.path.full();
            let committed = import_nar_reader(
                &handles.db.storage.nar_storage,
                &handles.graph,
                tokio::fs::File::open(staged.file.path()).await?,
                ImportInput {
                    store_path: &store_path,
                    verified: &staged.verified,
                    references: info.stated.references(),
                    deriver: info.stated.deriver(),
                    ca: info.stated.checked_content_address_text(),
                },
                producer,
            )
            .await?;
            drop(staged.permit);
            committed.signed
        }
    };

    for cache in signed {
        handles.db.events.publish(NarSigned {
            cache,
            hash: info.original.hash().to_string(),
        });
    }

    Ok(())
}

fn get_ssh_nar_upload_stored_content_producer(session: &Session) -> StoredContentProducer {
    StoredContentProducer::ProjectUserContentUpload {
        project: session.project.id,
        user: session.user.id,
        channel: CacheUploadChannel::SshNixStoreUpload,
    }
}

async fn find_verified_cached_path_with_existing_nar_for_upload(
    session: &Session,
    info: &StoredPathInfo,
) -> anyhow::Result<Option<MCachedPath>> {
    let db = &session.handles.db;
    let row = find_verified_cached_path_by_store_hash(
        &db.web_db,
        info.path.hash(),
        &info.nar_hash.as_sri().to_string(),
        &join_sorted_deduplicated_references_with_spaces(info.stated.references()),
        info.stated.checked_content_address_text(),
    )
    .await?;
    match row {
        Some(row)
            if db
                .storage
                .nar_storage
                .exists(&row.nar_object_storage_key)
                .await? =>
        {
            Ok(Some(row))
        }
        _ => Ok(None),
    }
}

async fn check_uploaded_nar_matches_stored_path(
    reader: impl AsyncRead + Unpin + Send,
    info: &StoredPathInfo,
    known: MCachedPath,
) -> anyhow::Result<Pending> {
    let (nar_hash, nar_size) =
        read_nar_stream_and_return_hash_and_size_without_storing(reader, info.nar_size).await?;
    anyhow::ensure!(
        nar_size == info.nar_size && nar_hash == info.nar_hash,
        "NAR hash of {} does not match",
        info.original
    );
    Ok(Pending::Known(known))
}

async fn claim_path(
    config: &RuntimeConfig,
    admission: &Arc<UploadAdmission>,
    hash: &str,
    size: u64,
) -> anyhow::Result<HeldPermit> {
    let wait = Duration::from_secs(config.upload.rest_wait_secs.get());
    admission
        .admit("ssh", ObjectKey::Nar(hash.to_owned()), size, wait)
        .await
        .ok_or_else(|| anyhow::anyhow!("upload capacity exhausted, retry later"))
}

async fn compress_into(
    mut reader: impl AsyncRead + Unpin + Send,
    path: &Path,
    announced: u64,
) -> anyhow::Result<(NarHash, u64, String, u64)> {
    let mut encoder = ZstdEncoder::with_quality(
        tokio::fs::File::create(path).await?,
        Level::Precise(gradient_wire::constants::NAR_ZSTD_LEVEL),
    );
    let mut raw = Context::new(Algorithm::SHA256);
    let mut nar_size = 0_u64;
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        nar_size += n as u64;
        anyhow::ensure!(nar_size <= announced, "NAR is longer than announced");
        raw.update(&buf[..n]);
        encoder.write_all(&buf[..n]).await?;
    }

    encoder.shutdown().await?;

    let mut compressed = tokio::fs::File::open(path).await?;
    let mut file = Context::new(Algorithm::SHA256);
    let mut file_size = 0_u64;
    loop {
        let n = compressed.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        file.update(&buf[..n]);
        file_size += n as u64;
    }

    Ok((
        NarHash::try_from(raw.finish())?,
        nar_size,
        Sha256::try_from(file.finish())?.as_sri().to_string(),
        file_size,
    ))
}

async fn read_nar_stream_and_return_hash_and_size_without_storing(
    mut reader: impl AsyncRead + Unpin + Send,
    announced: u64,
) -> anyhow::Result<(NarHash, u64)> {
    let mut raw = Context::new(Algorithm::SHA256);
    let mut nar_size = 0_u64;
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        nar_size += n as u64;
        anyhow::ensure!(nar_size <= announced, "NAR is longer than announced");
        raw.update(&buf[..n]);
    }
    Ok((NarHash::try_from(raw.finish())?, nar_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SshHandles;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH};
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_storage::admission::{Admitted, Limits};
    use gradient_test_support::content_address_and_store_path::{
        compute_derivation_store_path_and_text_content_address,
        flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes,
    };
    use gradient_test_support::state::TestState;
    use gradient_types::permissions::{Permission, mask_from};
    use gradient_types::{CacheId, MCachedPath};
    use gradient_util::nar::single_file_nar;
    use harmonia_protocol::valid_path_info::{UnkeyedValidPathInfo, ValidPathInfo};
    use harmonia_store_path::{StoreDir, StorePath};
    use sea_orm::DatabaseConnection;
    use std::sync::Arc;

    const NAR: &[u8] = b"nix-archive-1 stand-in bytes";

    async fn session(db: DatabaseConnection, permissions: i64) -> Arc<Session> {
        let state = TestState::new().web_db(db).build();
        let upload_admission = Arc::clone(&state.memory.upload_admission);
        session_with_upload_admission(state, permissions, upload_admission).await
    }

    async fn session_with_upload_admission(
        state: Arc<gradient_core::ServerState>,
        permissions: i64,
        upload_admission: Arc<UploadAdmission>,
    ) -> Arc<Session> {
        let session = Session::open(
            SshHandles {
                upload_admission,
                ..SshHandles::for_tests(&state)
            },
            gradient_fixtures::rows::user(),
            gradient_fixtures::rows::project(),
            permissions,
            vec![CacheId::now_v7()],
            true,
        )
        .await
        .unwrap();
        Arc::new(session)
    }

    fn info(nar: &[u8]) -> StoredPathInfo {
        let mapper = StorePathMapper::for_cache(Arc::new(IdentityMapping), CacheId::nil());
        let client = ValidPathInfo {
            path: StorePath::from_base_path("0123456789abcdfghijklmnpqrsvwxyz-hello")
                .expect("path"),
            info: UnkeyedValidPathInfo {
                deriver: None,
                nar_hash: NarHash::digest(nar),
                references: Default::default(),
                registration_time: None,
                nar_size: nar.len() as u64,
                ultimate: false,
                signatures: Default::default(),
                ca: None,
                store_dir: StoreDir::default(),
            },
        };
        StoredPathInfo::inbound(&mapper, &client).expect("stored info")
    }

    fn mock_database_with_no_project_cache_subscriptions() -> sea_orm::MockDatabase {
        mock_db().append_query_results([Vec::<gradient_entity::project_cache::Model>::new()])
    }

    #[tokio::test]
    async fn refuse_ssh_ng_staging_when_nar_size_plus_headroom_exceeds_free_cache_space() {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        let full = gradient_entity::cache::Model {
            max_storage_gb: 1,
            ..gradient_fixtures::rows::cache()
        };
        let free = gradient_db::caches::capacity::STORAGE_HEADROOM_BYTES
            + i64::try_from(info(NAR).nar_size).unwrap()
            - 1;
        let db = mock_db()
            .append_query_results([vec![gradient_fixtures::rows::project_cache_link(
                gradient_types::ids::ProjectCacheId::now_v7(),
                gradient_fixtures::rows::project().id,
                full.id,
                CacheSubscriptionMode::ReadWrite,
            )]])
            .append_query_results([vec![full.clone()]])
            .append_query_results([[std::collections::BTreeMap::from([(
                "total".to_owned(),
                sea_orm::Value::BigInt(Some(1024 * 1024 * 1024 - free)),
            )])]])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;

        let e = stage(&session, &info(NAR), NAR)
            .await
            .err()
            .expect("the NAR does not fit");

        assert!(e.to_string().contains(&full.name), "{e}");
        let stats = session.handles.upload_admission.stats();
        assert_eq!((stats.in_flight, stats.granted_total), (0, 0));
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "Storage limit checks only, no cached path lookup: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_nar_with_a_wrong_hash_is_rejected_before_any_row() {
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;
        let mut info = info(NAR);
        info.nar_hash = NarHash::digest(b"other");

        let e = import(&session, &info, NAR)
            .await
            .expect_err("hash mismatch");
        assert!(e.to_string().contains("NAR hash"), "{e}");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains("INSERT"), "{log}");
    }

    #[tokio::test]
    async fn a_nar_shorter_than_announced_is_rejected() {
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .into_connection();
        let session = session(db, mask_from(&[Permission::TriggerEvaluation])).await;
        let mut info = info(NAR);
        info.nar_size += 1;

        let e = import(&session, &info, NAR)
            .await
            .expect_err("size mismatch");
        assert!(e.to_string().contains("NAR size"), "{e}");
    }

    #[tokio::test]
    async fn copying_in_without_trigger_evaluation_is_refused() {
        let db = empty_db();
        let session = session(db, 0).await;

        let e = import(&session, &info(NAR), NAR)
            .await
            .expect_err("refused");
        assert!(e.to_string().contains("TriggerEvaluation"), "{e}");
    }

    #[tokio::test]
    async fn a_stream_longer_than_announced_stops_early() {
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .into_connection();
        let session = session(db, mask_from(&[Permission::TriggerEvaluation])).await;
        let mut info = info(NAR);
        info.nar_size = 4;

        let e = import(&session, &info, NAR).await.expect_err("too long");
        assert!(e.to_string().contains("longer than announced"), "{e}");
    }

    #[tokio::test]
    async fn ssh_upload_with_legacy_cached_path_other_content_is_stored_as_new_content_variant() {
        let existing = MCachedPath {
            hash: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            nar_object_storage_key: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            package: "hello".into(),
            nar_hash: NarHash::digest(b"original").as_sri().to_string(),
            file_hash: "sha256-x".into(),
            ..Default::default()
        };
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([vec![existing.clone()], vec![existing]])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;
        let mut upload = NAR;

        import(&session, &info(NAR), &mut upload)
            .await
            .expect("The upload stores a new variant.");

        assert!(upload.is_empty(), "the NAR bytes are drained");
        let legacy_key = "0123456789abcdfghijklmnpqrsvwxyz";
        let nar_storage = &session.handles.db.storage.nar_storage;
        assert!(!nar_storage.exists(legacy_key).await.unwrap());
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "The database received three queries. The first was a check of the storage limit for the session. Two lookups of cached paths followed, one before the upload turn and one after. A project upload can specify the project as producer and doesn't query the cache. The queries are {log:?}."
        );
    }

    #[tokio::test]
    async fn upload_with_matching_announced_hash_but_different_bytes_is_refused_and_nothing_is_signed()
     {
        let existing = MCachedPath {
            hash: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            nar_object_storage_key: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            package: "hello".into(),
            nar_hash: NarHash::digest(NAR).as_sri().to_string(),
            file_hash: "sha256-x".into(),
            ..Default::default()
        };
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([vec![existing.clone()], vec![existing]])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;
        let forged = vec![b'?'; NAR.len()];

        let e = import(&session, &info(NAR), forged.as_slice())
            .await
            .expect_err("the upload does not hold the stored content");

        assert!(e.to_string().contains("NAR hash"), "{e}");
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "No signature. Only a check of the storage limit and two reads: {log:?}"
        );
    }

    #[tokio::test]
    async fn an_upload_waits_for_the_upload_holding_its_path_and_then_checks_again() {
        let existing = MCachedPath {
            hash: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            nar_object_storage_key: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            package: "hello".into(),
            nar_hash: NarHash::digest(b"original").as_sri().to_string(),
            file_hash: "sha256-x".into(),
            ..Default::default()
        };
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([vec![existing.clone()], vec![existing]])
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection();
        let session = session(db, mask_from(&[Permission::TriggerEvaluation])).await;
        let info = info(NAR);
        let (holder, mut admitted) = session.handles.upload_admission.open_session("holder");
        holder.request(1, ObjectKey::Nar(info.path.hash().to_owned()), 1, false);
        let Some(Admitted { permit, .. }) = admitted.recv().await else {
            panic!("the holder leads the path");
        };
        let mut upload = NAR;

        {
            let waiting = import(&session, &info, &mut upload);
            tokio::pin!(waiting);
            assert!(
                tokio::time::timeout(Duration::from_millis(200), &mut waiting)
                    .await
                    .is_err(),
                "the upload waits while another upload holds the path"
            );
            drop(permit);
            waiting.await.expect("the stored content stays");
        }

        assert!(upload.is_empty(), "the NAR bytes are drained");
    }

    #[tokio::test]
    async fn an_upload_waits_for_the_permit_holder_to_store_the_path_and_then_returns_pending_known_without_uploading()
     {
        let key = "0123456789abcdfghijklmnpqrsvwxyz";
        let stored_by_holder = MCachedPath {
            hash: key.into(),
            nar_object_storage_key: key.into(),
            package: "hello".into(),
            nar_hash: gradient_util::nix_hash::normalize_nar_hash(
                &NarHash::digest(NAR).as_sri().to_string(),
            ),
            file_hash: "sha256-x".into(),
            server_computed_hashes_and_sizes: true,
            ..Default::default()
        };
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::new(), vec![stored_by_holder]])
            .into_connection();
        let session = session(db, mask_from(&[Permission::TriggerEvaluation])).await;
        let info = info(NAR);
        let upload_admission = Arc::clone(&session.handles.upload_admission);
        let (holder, mut admitted) = upload_admission.open_session("holder");
        holder.request(1, ObjectKey::Nar(info.path.hash().to_owned()), 1, false);
        let Some(Admitted { permit, .. }) = admitted.recv().await else {
            panic!("the holder leads the path");
        };
        let mut upload = NAR;

        let staged = {
            let waiting = stage(&session, &info, &mut upload);
            tokio::pin!(waiting);
            assert!(
                tokio::time::timeout(Duration::from_millis(200), &mut waiting)
                    .await
                    .is_err(),
                "the upload waits while another upload holds the path"
            );
            session
                .handles
                .db
                .storage
                .nar_storage
                .put(key, b"stored".to_vec())
                .await
                .unwrap();
            drop(permit);
            waiting.await.expect("the holder stored the path")
        };

        assert!(matches!(staged, Pending::Known(row) if row.hash == key));
        assert!(upload.is_empty(), "the NAR bytes are drained");
        assert_eq!(upload_admission.stats().in_flight, 0);
    }

    #[tokio::test]
    async fn ssh_upload_of_already_cached_path_with_upload_concurrency_one_and_only_slot_held_by_other_upload_succeeds_without_taking_slot()
     {
        let key = "0123456789abcdfghijklmnpqrsvwxyz";
        let existing = MCachedPath {
            hash: key.into(),
            nar_object_storage_key: key.into(),
            package: "hello".into(),
            nar_hash: gradient_util::nix_hash::normalize_nar_hash(
                &NarHash::digest(NAR).as_sri().to_string(),
            ),
            file_hash: "sha256-x".into(),
            server_computed_hashes_and_sizes: true,
            ..Default::default()
        };
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([vec![existing.clone()], vec![existing]])
            .into_connection();
        let upload_admission = UploadAdmission::new(Limits {
            concurrency: 1,
            bytes: u64::MAX,
        });
        let session = session_with_upload_admission(
            TestState::new().web_db(db.clone()).build(),
            mask_from(&[Permission::TriggerEvaluation]),
            Arc::clone(&upload_admission),
        )
        .await;
        session
            .handles
            .db
            .storage
            .nar_storage
            .put(key, b"stored".to_vec())
            .await
            .unwrap();
        let (other, mut other_admissions) = upload_admission.open_session("other");
        other.request(1, ObjectKey::Nar(OTHER_ORIGINAL_HASH.to_owned()), 1, false);
        let _held = other_admissions.recv().await;
        let info = info(NAR);

        let forged = vec![b'?'; NAR.len()];
        let e = tokio::time::timeout(
            Duration::from_secs(1),
            stage(&session, &info, forged.as_slice()),
        )
        .await
        .expect("A stored path doesn't use an upload slot.")
        .err()
        .expect("the upload does not hold the stored content");
        assert!(e.to_string().contains("NAR hash"), "{e}");
        let staged = tokio::time::timeout(Duration::from_secs(1), stage(&session, &info, NAR))
            .await
            .expect("A stored path doesn't use an upload slot.")
            .unwrap();

        assert!(matches!(staged, Pending::Known(row) if row.hash == key));
        let stats = upload_admission.stats();
        assert_eq!(
            (stats.in_flight, stats.granted_total),
            (1, 1),
            "The other upload is still the only upload in flight."
        );
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "No signature. Only a check of the storage limit and two reads: {log:?}"
        );
    }

    const OTHER_ORIGINAL_HASH: &str = "abcdfghijklmnpqrsvwxyz0123456789";

    fn stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
        mapping: Arc<dyn gradient_types::StorePathMapping>,
        base_path: &str,
        nar: &[u8],
        references: &[&str],
        content_address: Option<&str>,
    ) -> StoredPathInfo {
        let mapper = StorePathMapper::for_cache(mapping, CacheId::nil());
        let client = ValidPathInfo {
            path: StorePath::from_base_path(base_path).expect("path"),
            info: UnkeyedValidPathInfo {
                deriver: None,
                nar_hash: NarHash::digest(nar),
                references: references
                    .iter()
                    .map(|reference| StorePath::from_base_path(reference).expect("reference"))
                    .collect(),
                registration_time: None,
                nar_size: nar.len() as u64,
                ultimate: false,
                signatures: Default::default(),
                ca: content_address.map(|ca| ca.parse().expect("content address")),
                store_dir: StoreDir::default(),
            },
        };
        StoredPathInfo::inbound(&mapper, &client).expect("stored info")
    }

    #[test]
    fn test_ssh_upload_rejects_non_drv_deriver_and_accepts_drv_deriver_with_references_and_content_address()
     {
        let mapper =
            StorePathMapper::for_cache(Arc::new(gradient_entity::IdentityMapping), CacheId::nil());
        let path = format!("{EXAMPLE_ORIGINAL_HASH}-hello");
        let ca = "fixed:r:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf";
        let with_deriver = |deriver: &str| ValidPathInfo {
            path: StorePath::from_base_path(&path).expect("path"),
            info: UnkeyedValidPathInfo {
                deriver: Some(StorePath::from_base_path(deriver).expect("deriver")),
                nar_hash: NarHash::digest(b"nar"),
                references: [StorePath::from_base_path(&path).expect("reference")].into(),
                registration_time: None,
                nar_size: 3,
                ultimate: false,
                signatures: Default::default(),
                ca: Some(ca.parse().expect("content address")),
                store_dir: StoreDir::default(),
            },
        };

        let refused = StoredPathInfo::inbound(&mapper, &with_deriver(&path))
            .err()
            .expect("a deriver that is no .drv is refused");
        assert!(refused.to_string().contains("Deriver"), "{refused}");

        let accepted = StoredPathInfo::inbound(&mapper, &with_deriver(&format!("{path}.drv")))
            .expect("a .drv deriver is accepted");
        assert_eq!(accepted.stated.references(), std::slice::from_ref(&path));
        assert_eq!(
            accepted.stated.deriver(),
            Some(format!("/nix/store/{path}.drv").as_str())
        );
        assert_eq!(accepted.stated.checked_content_address_text(), Some(ca));
    }

    fn mock_database_with_no_cached_path_and_no_producer_cache() -> DatabaseConnection {
        mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .append_query_results([
                Vec::<std::collections::BTreeMap<String, sea_orm::Value>>::new(),
            ])
            .into_connection()
    }

    #[tokio::test]
    async fn an_ssh_upload_with_a_claimed_reference_that_does_not_occur_in_the_nar_is_rejected_before_any_store_or_record()
     {
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;
        let nar = single_file_nar(b"no references", false);
        let missing = format!("{OTHER_ORIGINAL_HASH}-bash");
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(IdentityMapping),
            &format!("{EXAMPLE_ORIGINAL_HASH}-hello"),
            &nar,
            &[&missing],
            None,
        );

        let e = import(&session, &info, nar.as_slice())
            .await
            .expect_err("the claimed reference is not in the NAR");

        assert!(e.to_string().contains(&missing), "{e}");
        let nar_storage = &session.handles.db.storage.nar_storage;
        assert!(!nar_storage.exists(EXAMPLE_ORIGINAL_HASH).await.unwrap());
        assert_eq!(
            std::fs::read_dir(session.handles.db.config.server.nar_upload_partial_dir())
                .unwrap()
                .count(),
            0
        );
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "No records with only a check of the storage limit and two lookups: {log:?}"
        );
    }

    #[tokio::test]
    async fn ssh_nar_upload_with_all_claimed_references_present_is_stored() {
        let session = session(
            mock_database_with_no_cached_path_and_no_producer_cache(),
            mask_from(&[Permission::TriggerEvaluation]),
        )
        .await;
        let own = format!("{EXAMPLE_ORIGINAL_HASH}-hello");
        let other = format!("{OTHER_ORIGINAL_HASH}-bash");
        let nar = single_file_nar(
            format!("#!/nix/store/{other}/bin/sh\n/nix/store/{own}").as_bytes(),
            true,
        );
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(IdentityMapping),
            &own,
            &nar,
            &[&other, &own],
            None,
        );

        import(&session, &info, nar.as_slice())
            .await
            .expect("every claimed reference is in the NAR");
    }

    #[tokio::test]
    async fn ssh_upload_is_stored_when_store_path_mapping_changes_hash_parts_and_verifier_sees_references_sent_by_nix_client()
     {
        let session = session(
            mock_database_with_no_cached_path_and_no_producer_cache(),
            mask_from(&[Permission::TriggerEvaluation]),
        )
        .await;
        let dependency = format!("{EXAMPLE_ORIGINAL_HASH}-bash");
        let nar = single_file_nar(format!("#!/nix/store/{dependency}/bin/sh").as_bytes(), true);
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(gradient_entity::RotatedHashMapping),
            &format!("{OTHER_ORIGINAL_HASH}-hello"),
            &nar,
            &[&dependency],
            None,
        );
        assert_eq!(
            info.stated.references(),
            [format!("{EXAMPLE_STORED_HASH}-bash")]
        );

        import(&session, &info, nar.as_slice())
            .await
            .expect("the verifier sees the reference as the client sent it");
    }

    #[tokio::test]
    async fn ssh_upload_of_real_derivation_nar_with_its_reference_is_stored() {
        let session = session(
            mock_database_with_no_cached_path_and_no_producer_cache(),
            mask_from(&[Permission::TriggerEvaluation]),
        )
        .await;
        let base = |path: &str| path.trim().trim_start_matches("/nix/store/").to_owned();
        let nar = single_file_nar(include_bytes!("../testdata/input-addressed-top.drv"), false);
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(IdentityMapping),
            &base(include_str!("../testdata/input-addressed-top.drv.path")),
            &nar,
            &[&base(include_str!(
                "../testdata/input-addressed-dep.drv.path"
            ))],
            None,
        );

        import(&session, &info, nar.as_slice())
            .await
            .expect("a derivation names its inputs in its text");
    }

    async fn assert_refused_ssh_upload_stored_nothing(
        session: &Session,
        db: DatabaseConnection,
        hash: &str,
    ) {
        let nar_storage = &session.handles.db.storage.nar_storage;
        assert!(!nar_storage.exists(hash).await.unwrap());
        assert_eq!(
            std::fs::read_dir(session.handles.db.config.server.nar_upload_partial_dir())
                .unwrap()
                .count(),
            0
        );
        let log = db.into_transaction_log();
        assert_eq!(
            log.len(),
            3,
            "No records with only a check of the storage limit and two lookups: {log:?}"
        );
    }

    fn test_derivation_with_changed_system_and_real_paths() -> (Vec<u8>, String, String) {
        let base = |path: &str| path.trim().trim_start_matches("/nix/store/").to_owned();
        (
            include_str!("../testdata/input-addressed-top.drv")
                .replace("x86_64-linux", "aarch64-linux")
                .into_bytes(),
            base(include_str!("../testdata/input-addressed-top.drv.path")),
            base(include_str!("../testdata/input-addressed-dep.drv.path")),
        )
    }

    #[tokio::test]
    async fn ssh_upload_rejects_drv_with_altered_text_stating_original_content_address_and_stores_nothing()
     {
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;
        let (changed, own, input) = test_derivation_with_changed_system_and_real_paths();
        let original = compute_derivation_store_path_and_text_content_address(
            "top.drv",
            include_bytes!("../testdata/input-addressed-top.drv"),
            std::slice::from_ref(&input),
        );
        assert_eq!(original.store_path, format!("/nix/store/{own}"));
        let nar = single_file_nar(&changed, false);
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(IdentityMapping),
            &own,
            &nar,
            &[&input],
            Some(&original.content_address),
        );

        import(&session, &info, nar.as_slice())
            .await
            .expect_err("text doesn't match declared content address");

        assert_refused_ssh_upload_stored_nothing(&session, db, &own[..32]).await;
    }

    #[tokio::test]
    async fn test_ssh_upload_rejects_drv_without_content_address_when_path_does_not_follow_from_text()
     {
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;
        let (changed, own, input) = test_derivation_with_changed_system_and_real_paths();
        let nar = single_file_nar(&changed, false);
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(IdentityMapping),
            &own,
            &nar,
            &[&input],
            None,
        );

        import(&session, &info, nar.as_slice())
            .await
            .expect_err("store path does not derive from text");

        assert_refused_ssh_upload_stored_nothing(&session, db, &own[..32]).await;
    }

    #[tokio::test]
    async fn ssh_upload_rejects_fixed_output_path_with_mismatched_content_address_hash_and_stores_nothing()
     {
        let db = mock_database_with_no_project_cache_subscriptions()
            .append_query_results([Vec::<MCachedPath>::new(), Vec::new()])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation])).await;
        let claimed = flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
            "source.tar.gz",
            b"the tarball the hash was taken from",
        );
        let own = claimed.store_path.trim_start_matches("/nix/store/");
        let nar = single_file_nar(b"other tarball bytes", false);
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(IdentityMapping),
            own,
            &nar,
            &[],
            Some(&claimed.content_address),
        );

        import(&session, &info, nar.as_slice())
            .await
            .expect_err("contents differ from the specified content address");

        assert_refused_ssh_upload_stored_nothing(&session, db, &own[..32]).await;
    }

    #[tokio::test]
    async fn ssh_upload_fixed_output_file_with_correct_address_is_stored() {
        let session = session(
            mock_database_with_no_cached_path_and_no_producer_cache(),
            mask_from(&[Permission::TriggerEvaluation]),
        )
        .await;
        let contents = b"tarball bytes";
        let fixed_output =
            flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
                "source.tar.gz",
                contents,
            );
        let nar = single_file_nar(contents, false);
        let info = stored_path_info_for_ssh_upload_with_references_and_store_path_mapper(
            Arc::new(IdentityMapping),
            fixed_output.store_path.trim_start_matches("/nix/store/"),
            &nar,
            &[],
            Some(&fixed_output.content_address),
        );

        import(&session, &info, nar.as_slice())
            .await
            .expect("content and stated content address agree");
    }
}
