/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct StorageArgs {
    #[arg(long, env = "GRADIENT_STORE_PATH")]
    pub store_path: Option<String>,
    #[arg(long, env = "GRADIENT_BASE_PATH", default_value = ".")]
    pub base_path: String,
    #[arg(long, env = "GRADIENT_STATE_FILE")]
    pub state_file: Option<String>,
    #[arg(long, env = "GRADIENT_DELETE_STATE", default_value = "true")]
    pub delete_state: bool,
    #[arg(long, env = "GRADIENT_KEEP_EVALUATIONS", default_value = "0")]
    pub keep_evaluations: usize,
    /// TTL in hours for cached NAR files that have not been fetched recently.
    /// When expired the NAR is removed from storage and its GC root is deleted.
    /// Set to 0 to disable (default).
    #[arg(long, env = "GRADIENT_NAR_TTL_HOURS", default_value_t = 0)]
    pub nar_ttl_hours: u64,
    /// Grace period in hours before the GC pass deletes a `derivation` row
    /// that no longer has any referencing `build` rows. The grace lets rapid
    /// re-evaluations reuse a freshly-orphaned derivation without
    /// re-inserting it. Set to 0 to GC immediately.
    #[arg(
        long,
        env = "GRADIENT_KEEP_ORPHAN_DERIVATIONS_HOURS",
        default_value_t = 24
    )]
    pub keep_orphan_derivations_hours: i64,
}

impl Default for StorageArgs {
    fn default() -> Self {
        Self {
            store_path: None,
            base_path: ".".into(),
            state_file: None,
            delete_state: true,
            keep_evaluations: 0,
            nar_ttl_hours: 0,
            keep_orphan_derivations_hours: 24,
        }
    }
}
