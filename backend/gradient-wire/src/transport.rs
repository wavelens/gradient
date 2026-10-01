/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::QueryMode;

/// How a NAR crosses between worker and storage: over the proto stream through
/// the server, or straight to object storage on a presigned URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Passthrough,
    Presigned,
}

/// Whether a query may leave our cache. Only a caller that said `external` ever
/// does: a build's inputs are here or the build fails, and putting them here is a
/// Substitute's job, so a Pull without the flag is answered from our rows alone.
pub fn may_consult_upstream_caches(mode: QueryMode, external: bool) -> bool {
    external && !matches!(mode, QueryMode::Push)
}

/// An external query names exactly one path: the probe is per path and the caller
/// is asking about one output.
pub fn external_arity_ok(external: bool, paths: usize) -> bool {
    !external || paths == 1
}

/// Download transport for a cached path: passthrough unless the store can presign,
/// the object is confirmed there, and it is over the threshold.
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
