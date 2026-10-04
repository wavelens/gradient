/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::config::WorkerConfig;
use crate::executor::JobExecutor;

fn user_state_dirs(config: &WorkerConfig, xdg_cache_home: Option<&str>) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from(config.eval_cache_dir()),
        config.nar_partial_dir(),
    ];
    dirs.extend(xdg_cache_home.map(|cache| Path::new(cache).join("nix")));
    dirs
}

pub(super) async fn forget_previous_user(
    executor: &JobExecutor,
    config: &WorkerConfig,
) -> Result<()> {
    executor.evaluator.resolver().release_idle().await;
    let xdg_cache_home = std::env::var("XDG_CACHE_HOME").ok();
    for dir in user_state_dirs(config, xdg_cache_home.as_deref()) {
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("wiping {}", dir.display())),
        }
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("recreating {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handover_wipes_the_eval_cache_partial_nars_and_the_nix_fetcher_cache() {
        let config = WorkerConfig::default();
        let dirs = user_state_dirs(&config, Some("/var/lib/gradient-worker/www/.cache"));
        assert_eq!(
            dirs,
            [
                PathBuf::from("/var/lib/gradient-worker/eval-cache"),
                PathBuf::from("/var/lib/gradient-worker/nar-partial"),
                PathBuf::from("/var/lib/gradient-worker/www/.cache/nix"),
            ]
        );
    }
}
