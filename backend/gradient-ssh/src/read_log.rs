/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::session::Session;
use gradient_core::build_logs::{BuildLog, find_build_log};
use gradient_db::cache_paths::served_path;
use gradient_types::OriginalStorePath;

pub async fn read_log(session: &Session, path: &str) -> anyhow::Result<Option<String>> {
    let Some(drv) = derivation_of(session, OriginalStorePath::parse(path)?).await? else {
        return Ok(None);
    };

    let db = &session.handles.db;
    let caches = gradient_db::caches::by_ids(&db.web_db, &session.caches).await?;
    let found = find_build_log(
        &db.web_db,
        db.storage.log_storage.as_ref(),
        &caches,
        &session.mapper,
        &drv,
    )
    .await?;

    Ok(found.map(BuildLog::into_text))
}

async fn derivation_of(
    session: &Session,
    path: OriginalStorePath,
) -> anyhow::Result<Option<OriginalStorePath>> {
    if path.name().ends_with(".drv") {
        return Ok(Some(path));
    }

    let stored = session.mapper.inbound(&path);
    let served = served_path(&session.handles.db.web_db, &session.caches, stored.hash()).await?;
    let Some(deriver) = served.and_then(|served| served.deriver) else {
        return Ok(None);
    };

    Ok(Some(OriginalStorePath::parse(
        &session.mapper.outbound_text(&deriver)?,
    )?))
}
