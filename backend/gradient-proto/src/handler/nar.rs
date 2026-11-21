/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::DbContext;
use gradient_db::store_path_content_variant_trust::find_worker_credentials_for_project_job;
use gradient_graph::{Graph, NarCommit, NarCommitContent, StoredContentProducer};
use gradient_storage::ServerComputedNarAndCompressedFileHashesAndSizes;
use gradient_types::events::cache::NarSigned;
use gradient_types::ids::{DispatchedJobId, ProjectId};
use gradient_types::*;
use std::sync::Arc;

pub(super) struct NarUploadRecord<'a> {
    pub verified: &'a ServerComputedNarAndCompressedFileHashesAndSizes,
    pub references: &'a [String],
    pub deriver: Option<&'a str>,
    pub ca: Option<&'a str>,
}

pub(super) struct NarUploadWorkerSession<'a> {
    pub worker_id: &'a str,
    pub project: ProjectId,
    pub dispatched_job: Option<DispatchedJobId>,
}

pub(super) async fn mark_nar_stored(
    graph: &Graph,
    db: &DbContext,
    worker: &NarUploadWorkerSession<'_>,
    store_path: &str,
    record: &NarUploadRecord<'_>,
) -> anyhow::Result<()> {
    let Ok(path) = StorePath::parse_stored(store_path) else {
        return Ok(());
    };

    let credentials =
        find_worker_credentials_for_project_job(&db.worker_db, worker.worker_id, worker.project)
            .await?;
    let committed = graph
        .commit_nar(NarCommit {
            store_path: store_path.to_owned(),
            content: NarCommitContent::ServerComputedNarAndCompressedFileHashesAndSizes(
                record.verified.clone(),
            ),
            references: record.references.to_vec(),
            deriver: record.deriver.map(str::to_owned),
            ca: record.ca.map(str::to_owned),
            producer: StoredContentProducer::Worker {
                project: worker.project,
                credentials,
                dispatched_job: worker.dispatched_job,
            },
        })
        .await?;

    for event in signed_events(&db.store_path_mapping, &committed.signed, &path) {
        db.events.publish(event);
    }

    Ok(())
}

fn signed_events(
    mapping: &Arc<dyn StorePathMapping>,
    caches: &[CacheId],
    path: &StorePath,
) -> Vec<NarSigned> {
    caches
        .iter()
        .map(|&cache| NarSigned {
            cache,
            hash: StorePathMapper::for_cache(Arc::clone(mapping), cache)
                .outbound(path)
                .hash()
                .to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};

    #[test]
    fn a_signed_nar_is_announced_with_its_original_hash() {
        let mapping: Arc<dyn StorePathMapping> = Arc::new(RotatedHashMapping);
        let path = StorePath::parse_stored(&format!("/nix/store/{EXAMPLE_STORED_HASH}-hello"))
            .expect("a stored path");
        let caches = vec![CacheId::now_v7(), CacheId::now_v7()];

        let events = signed_events(&mapping, &caches, &path);

        assert_eq!(events.len(), 2);
        for (event, cache) in events.iter().zip(&caches) {
            assert_eq!(event.cache, *cache);
            assert_eq!(event.hash, EXAMPLE_ORIGINAL_HASH);
        }
    }
}
