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
    #[arg(long, env = "GRADIENT_KEEP_EVALUATIONS", default_value = "30")]
    pub keep_evaluations: usize,
    /// TTL in hours for cached NAR files that have not been fetched recently.
    /// When expired the NAR is removed from storage and its GC root is deleted.
    /// Defaults to 336 (2 weeks). Set to 0 to disable.
    #[arg(long, env = "GRADIENT_NAR_TTL_HOURS", default_value_t = 336)]
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
    /// Target uncompressed size in bytes for each zstd log chunk written when a
    /// build finalizes. Chunks split on line boundaries, so an over-long line
    /// may exceed this. Defaults to 262144 (256 KiB).
    #[arg(long, env = "GRADIENT_LOG_CHUNK_BYTES", default_value_t = 262144)]
    pub log_chunk_bytes: usize,
}

impl Default for StorageArgs {
    fn default() -> Self {
        Self {
            store_path: None,
            base_path: ".".into(),
            state_file: None,
            delete_state: true,
            keep_evaluations: 30,
            nar_ttl_hours: 336,
            keep_orphan_derivations_hours: 24,
            log_chunk_bytes: 262144,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn default_keep_evaluations_is_thirty() {
        assert_eq!(StorageArgs::default().keep_evaluations, 30);
    }

    #[test]
    fn default_nar_ttl_hours_is_two_weeks() {
        assert_eq!(StorageArgs::default().nar_ttl_hours, 336);
    }

    #[derive(Parser, Debug)]
    struct StorageOnlyCli {
        #[command(flatten)]
        storage: StorageArgs,
    }

    #[test]
    fn clap_default_keep_evaluations_is_thirty() {
        let parsed = StorageOnlyCli::try_parse_from(["test"]).unwrap();
        assert_eq!(parsed.storage.keep_evaluations, 30);
    }

    #[test]
    fn clap_default_nar_ttl_hours_is_two_weeks() {
        let parsed = StorageOnlyCli::try_parse_from(["test"]).unwrap();
        assert_eq!(parsed.storage.nar_ttl_hours, 336);
    }
}
