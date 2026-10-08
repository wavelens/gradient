/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::Session;
use anyhow::Context as _;
use async_compression::Level;
use async_compression::tokio::write::ZstdEncoder;
use gradient_db::permissions::Permission;
use gradient_graph::NarCommit;
use gradient_proto::import::{ImportInput, SignTargets, import_nar_reader, stored_path};
use gradient_storage::admission::{Admission, HeldPermit, ObjectKey};
use gradient_types::events::cache::NarSigned;
use gradient_types::*;
use gradient_util::nix_hash::normalize_nar_hash;
use harmonia_protocol::valid_path_info::ValidPathInfo;
use harmonia_store_path_info::NarHash;
use harmonia_utils_hash::{Algorithm, Context, HashFormat as _, Sha256};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tracing::debug;

pub enum Pending {
    Known(MCachedPath),
    OtherContentStored,
    New(Staged),
}

pub struct Staged {
    file: tempfile::NamedTempFile,
    nar_hash: NarHash,
    nar_size: u64,
    file_hash: String,
    file_size: u64,
    permit: HeldPermit,
}

pub async fn import(
    session: &Session,
    info: &ValidPathInfo,
    reader: impl AsyncRead + Unpin + Send,
) -> anyhow::Result<()> {
    let pending = stage(session, info, reader).await?;
    commit(session, info, pending).await
}

pub async fn stage(
    session: &Session,
    info: &ValidPathInfo,
    mut reader: impl AsyncRead + Unpin + Send,
) -> anyhow::Result<Pending> {
    anyhow::ensure!(
        session.may(Permission::TriggerEvaluation),
        "copying into {} needs TriggerEvaluation",
        session.project.name
    );

    let state = &session.state;
    let hash = info.path.hash().to_string();
    let claim = claim_path(state, &hash, info.info.nar_size).await?;
    if let Some(known) = stored_path(&state.web_db, &hash).await? {
        if let Some(claim) = claim {
            claim.committed();
        }

        tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
        let announced = normalize_nar_hash(&info.info.nar_hash.as_sri().to_string());
        if known.nar_hash.as_deref().map(normalize_nar_hash) == Some(announced) {
            return Ok(Pending::Known(known));
        }

        debug!(path = %info.path, "other content is stored for this path; dropping the upload");
        return Ok(Pending::OtherContentStored);
    }

    let permit = claim.with_context(|| {
        format!(
            "{} was stored by another upload and removed again, retry",
            info.path
        )
    })?;
    let dir = state.config.server.nar_upload_partial_dir();
    tokio::fs::create_dir_all(&dir).await?;
    let file = tempfile::NamedTempFile::new_in(&dir)?;
    let (nar_hash, nar_size, file_hash, file_size) =
        compress_into(reader, file.path(), info.info.nar_size).await?;
    anyhow::ensure!(
        nar_size == info.info.nar_size,
        "NAR size of {} does not match",
        info.path
    );
    anyhow::ensure!(
        nar_hash == info.info.nar_hash,
        "NAR hash of {} does not match",
        info.path
    );

    Ok(Pending::New(Staged {
        file,
        nar_hash,
        nar_size,
        file_hash,
        file_size,
        permit,
    }))
}

pub async fn commit(
    session: &Session,
    info: &ValidPathInfo,
    pending: Pending,
) -> anyhow::Result<()> {
    let state = &session.state;
    let targets = SignTargets::ProjectCaches(session.project.id);
    let signed = match pending {
        Pending::Known(known) => {
            state
                .graph
                .commit_nar(NarCommit::from_stored_row(&known, targets))
                .await?
                .signed
        }
        Pending::OtherContentStored => Vec::new(),
        Pending::New(staged) => {
            let store_path = format!("/nix/store/{}", info.path);
            let references: Vec<String> =
                info.info.references.iter().map(|r| r.to_string()).collect();
            let deriver = info
                .info
                .deriver
                .as_ref()
                .map(|d| format!("/nix/store/{d}"));
            let ca = info.info.ca.as_ref().map(|c| c.to_string());
            let nar_hash = staged.nar_hash.as_sri().to_string();
            let committed = import_nar_reader(
                &state.web_db,
                &state.nar_storage,
                &state.graph,
                tokio::fs::File::open(staged.file.path()).await?,
                ImportInput {
                    store_path: &store_path,
                    file_hash: &staged.file_hash,
                    file_size: staged.file_size as i64,
                    nar_size: staged.nar_size as i64,
                    nar_hash: &nar_hash,
                    references: &references,
                    deriver: deriver.as_deref(),
                    ca: ca.as_deref(),
                },
                targets,
            )
            .await?;
            staged.permit.committed();
            committed.signed
        }
    };

    for cache in signed {
        state.events.publish(NarSigned {
            cache,
            hash: info.path.hash().to_string(),
        });
    }

    Ok(())
}

async fn claim_path(
    state: &gradient_core::ServerState,
    hash: &str,
    size: u64,
) -> anyhow::Result<Option<HeldPermit>> {
    let wait = Duration::from_secs(state.config.upload.rest_wait_secs);
    match state
        .upload_admission
        .admit("ssh", ObjectKey::Nar(hash.to_owned()), size, wait)
        .await
    {
        Some(Admission::Granted(permit)) => Ok(Some(permit)),
        Some(Admission::AlreadyCommitted) => Ok(None),
        None => anyhow::bail!("upload capacity exhausted, retry later"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_db::permissions::{Permission, mask_from};
    use gradient_storage::admission::Admitted;
    use gradient_types::{CacheId, MCachedPath};
    use harmonia_protocol::valid_path_info::UnkeyedValidPathInfo;
    use harmonia_store_path::{StoreDir, StorePath};
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};
    use std::sync::Arc;

    const NAR: &[u8] = b"nix-archive-1 stand-in bytes";

    fn session(db: DatabaseConnection, permissions: i64) -> Arc<Session> {
        Arc::new(Session {
            state: gradient_test_support::state::test_state_web(db),
            user: gradient_test_support::fixtures::user(),
            project: gradient_test_support::fixtures::project(),
            permissions,
            caches: vec![CacheId::now_v7()],
        })
    }

    fn info(nar: &[u8]) -> ValidPathInfo {
        ValidPathInfo {
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
        }
    }

    #[tokio::test]
    async fn a_nar_with_a_wrong_hash_is_rejected_before_any_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation]));
        let mut info = info(NAR);
        info.info.nar_hash = NarHash::digest(b"other");

        let e = import(&session, &info, NAR)
            .await
            .expect_err("hash mismatch");
        assert!(e.to_string().contains("NAR hash"), "{e}");
        let log = format!("{:?}", db.into_transaction_log());
        assert!(!log.contains("INSERT"), "{log}");
    }

    #[tokio::test]
    async fn a_nar_shorter_than_announced_is_rejected() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .into_connection();
        let session = session(db, mask_from(&[Permission::TriggerEvaluation]));
        let mut info = info(NAR);
        info.info.nar_size += 1;

        let e = import(&session, &info, NAR)
            .await
            .expect_err("size mismatch");
        assert!(e.to_string().contains("NAR size"), "{e}");
    }

    #[tokio::test]
    async fn copying_in_without_trigger_evaluation_is_refused() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let session = session(db, 0);

        let e = import(&session, &info(NAR), NAR)
            .await
            .expect_err("refused");
        assert!(e.to_string().contains("TriggerEvaluation"), "{e}");
    }

    #[tokio::test]
    async fn a_stream_longer_than_announced_stops_early() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .into_connection();
        let session = session(db, mask_from(&[Permission::TriggerEvaluation]));
        let mut info = info(NAR);
        info.info.nar_size = 4;

        let e = import(&session, &info, NAR).await.expect_err("too long");
        assert!(e.to_string().contains("longer than announced"), "{e}");
    }

    #[tokio::test]
    async fn a_known_path_with_other_content_is_drained_and_dropped() {
        let existing = MCachedPath {
            hash: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            package: "hello".into(),
            nar_hash: Some(NarHash::digest(b"original").as_sri().to_string()),
            file_hash: Some("sha256-x".into()),
            confirmed: true,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![existing]])
            .into_connection();
        let session = session(db.clone(), mask_from(&[Permission::TriggerEvaluation]));
        let mut upload = NAR;

        import(&session, &info(NAR), &mut upload)
            .await
            .expect("the stored content stays and the upload is accepted");

        assert!(upload.is_empty(), "the NAR bytes are drained");
        let log = db.into_transaction_log();
        assert_eq!(log.len(), 1, "only the lookup, nothing signed: {log:?}");
    }

    #[tokio::test]
    async fn an_upload_waits_for_the_upload_holding_its_path_and_then_checks_again() {
        let existing = MCachedPath {
            hash: "0123456789abcdfghijklmnpqrsvwxyz".into(),
            package: "hello".into(),
            nar_hash: Some(NarHash::digest(b"original").as_sri().to_string()),
            file_hash: Some("sha256-x".into()),
            confirmed: true,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![existing]])
            .into_connection();
        let session = session(db, mask_from(&[Permission::TriggerEvaluation]));
        let info = info(NAR);
        let (holder, mut admitted) = session.state.upload_admission.open_session("holder");
        holder.request(1, ObjectKey::Nar(info.path.hash().to_string()), 1, false);
        let Some(Admitted::Granted { permit, .. }) = admitted.recv().await else {
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
            permit.committed();
            waiting.await.expect("the stored content stays");
        }

        assert!(upload.is_empty(), "the NAR bytes are drained");
    }
}
