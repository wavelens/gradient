/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::upstream_source::{fetch_upstream_log, substitution_sources};
use gradient_db::WebDb;
use gradient_db::caches::upstream::pull_through_upstream_caches;
use gradient_storage::LogStorage;
use gradient_types::{CacheId, MCache, OriginalStorePath, StorePath, StorePathMapper};
use sea_orm::DbErr;

#[derive(Debug, PartialEq, Eq)]
pub enum BuildLog {
    Stored(String),
    Upstream(String),
}

impl BuildLog {
    pub fn into_text(self) -> String {
        match self {
            BuildLog::Stored(text) | BuildLog::Upstream(text) => text,
        }
    }
}

pub async fn find_build_log(
    db: &WebDb,
    log_storage: &dyn LogStorage,
    caches: &[MCache],
    mapper: &StorePathMapper,
    drv: &OriginalStorePath,
) -> Result<Option<BuildLog>, DbErr> {
    let stored = mapper.inbound(drv);
    for cache in caches {
        if let Some(text) = stored_log(db, log_storage, cache.id, &stored).await? {
            return Ok(Some(BuildLog::Stored(text)));
        }
    }

    Ok(upstream_log(db, caches, drv).await?.map(BuildLog::Upstream))
}

// Failed builds are not excluded on purpose: a rebuild that failed after the first success still
// carries the most recent log.
async fn stored_log(
    db: &WebDb,
    log_storage: &dyn LogStorage,
    cache: CacheId,
    drv: &StorePath,
) -> Result<Option<String>, DbErr> {
    let Some(drv_name) = drv
        .name()
        .strip_suffix(".drv")
        .filter(|name| !name.is_empty())
    else {
        return Ok(None);
    };

    let Some(derivation) =
        gradient_db::cached_paths::derivation_signed_in_cache(db, cache, drv.hash(), drv_name)
            .await?
    else {
        return Ok(None);
    };

    let Some(shared_build) = gradient_db::derivation_builds::by_derivation(db, derivation).await?
    else {
        return Ok(None);
    };

    let Some(attempt) =
        gradient_db::scheduling::build_attempt::latest_attempt_id(db, shared_build.id).await?
    else {
        return Ok(None);
    };

    Ok(log_storage
        .read(attempt)
        .await
        .ok()
        .filter(|text| !text.is_empty()))
}

async fn upstream_log(
    db: &WebDb,
    caches: &[MCache],
    drv: &OriginalStorePath,
) -> Result<Option<String>, DbErr> {
    for cache in caches {
        let upstream_caches = pull_through_upstream_caches(db, cache).await?;
        let sources = substitution_sources(&upstream_caches);
        if let Some(text) = fetch_upstream_log(&sources, &drv.base()).await {
            return Ok(Some(text));
        }
    }

    Ok(None)
}
