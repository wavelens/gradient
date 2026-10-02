/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::QueryMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Passthrough,
    Presigned,
}

/// Only a caller that said `external` can leave our cache. A build's inputs are in our cache or the
/// build is failing. Putting them there is a Substitute's job.
pub fn may_consult_upstream_caches(mode: QueryMode, external: bool) -> bool {
    external && !matches!(mode, QueryMode::Push)
}

pub fn external_arity_ok(external: bool, paths: usize) -> bool {
    !external || paths == 1
}

pub fn pull_transport(
    confirmed: bool,
    file_size: u64,
    small_nar_bytes: u64,
    presigner: bool,
) -> Transport {
    if presigner && confirmed && file_size > small_nar_bytes {
        Transport::Presigned
    } else {
        Transport::Passthrough
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_transport_passes_through_unconfirmed_small_and_presignerless_paths() {
        let threshold = 1024 * 1024;
        assert_eq!(
            pull_transport(true, threshold + 1, threshold, true),
            Transport::Presigned
        );
        assert_eq!(
            pull_transport(false, threshold + 1, threshold, true),
            Transport::Passthrough
        );
        assert_eq!(
            pull_transport(true, threshold, threshold, true),
            Transport::Passthrough
        );
        assert_eq!(
            pull_transport(true, threshold + 1, threshold, false),
            Transport::Passthrough
        );
    }

    #[test]
    fn only_an_external_pull_or_normal_query_leaves_our_cache() {
        assert!(may_consult_upstream_caches(QueryMode::Pull, true));
        assert!(may_consult_upstream_caches(QueryMode::Normal, true));
        assert!(!may_consult_upstream_caches(QueryMode::Push, true));
        assert!(!may_consult_upstream_caches(QueryMode::Pull, false));
        assert!(!may_consult_upstream_caches(QueryMode::Normal, false));
    }

    #[test]
    fn an_external_query_names_exactly_one_path() {
        assert!(external_arity_ok(false, 0) && external_arity_ok(false, 200));
        assert!(external_arity_ok(true, 1));
        assert!(!external_arity_ok(true, 0) && !external_arity_ok(true, 2));
    }
}
