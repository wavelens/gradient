/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::config::WorkerConfig;
use crate::executor::JobExecutor;

pub(super) async fn forget_eval_cache(executor: &JobExecutor, config: &WorkerConfig) -> Result<()> {
    executor.evaluator.resolver().release_idle().await;
    let dir = PathBuf::from(config.eval_cache_dir());
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("wiping {}", dir.display())),
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("recreating {}", dir.display()))
}
