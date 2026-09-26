/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::constants::MULTIPART_NAR_BYTES;
use crate::types::QueryMode;

/// How a NAR crosses between worker and storage: over the proto stream through
/// the server, or straight to object storage on a presigned URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Relay,
    Presigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushTransport {
    Relay,
    Put,
    Multipart(u64),
}

/// Upload transport for an uncached path: relay unless the store can presign
/// and the NAR is over the small-NAR threshold; past `MULTIPART_NAR_BYTES` a
/// single PUT could hit S3's 5 GiB cap, so the upload goes in parts. Parts are
/// sized from the NAR, so an unknown size gets a single PUT.
pub fn push_transport(
    nar_size: Option<u64>,
    small_nar_bytes: u64,
    presigner: bool,
) -> PushTransport {
    match nar_size {
        _ if !presigner => PushTransport::Relay,
        Some(size) if size <= small_nar_bytes => PushTransport::Relay,
        Some(size) if size > MULTIPART_NAR_BYTES => PushTransport::Multipart(size),
        _ => PushTransport::Put,
    }
}

/// Whether a query may leave our cache. Only a caller that said `external` ever
/// does: a build's inputs are here or the build fails, and putting them here is a
/// Substitute's job, so a Pull without the flag is answered from our rows alone.
pub fn may_consult_upstreams(mode: QueryMode, external: bool) -> bool {
    external && !matches!(mode, QueryMode::Push)
}

/// An external query names exactly one path: the probe is per path and the caller
/// is asking about one output.
pub fn external_arity_ok(external: bool, paths: usize) -> bool {
    !external || paths == 1
}

/// Download transport for a cached path: relay unless the store can presign,
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
        Transport::Relay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_transport_relays_small_nars_and_presigns_large_ones() {
        let threshold = 1024 * 1024;
        assert_eq!(
            push_transport(Some(1024), threshold, true),
            PushTransport::Relay
        );
        assert_eq!(
            push_transport(Some(threshold), threshold, true),
            PushTransport::Relay
        );
        assert_eq!(
            push_transport(Some(threshold + 1), threshold, true),
            PushTransport::Put
        );
        assert_eq!(
            push_transport(Some(MULTIPART_NAR_BYTES), threshold, true),
            PushTransport::Put
        );
        assert_eq!(
            push_transport(Some(MULTIPART_NAR_BYTES + 1), threshold, true),
            PushTransport::Multipart(MULTIPART_NAR_BYTES + 1)
        );
        assert_eq!(push_transport(None, threshold, false), PushTransport::Relay);
    }

    #[test]
    fn an_unknown_size_gets_a_single_put_never_a_multipart_upload() {
        let threshold = 1024 * 1024;
        assert_eq!(push_transport(None, threshold, true), PushTransport::Put);
    }

    #[test]
    fn pull_transport_relays_unconfirmed_small_and_presignerless_paths() {
        let threshold = 1024 * 1024;
        assert_eq!(
            pull_transport(true, threshold + 1, threshold, true),
            Transport::Presigned
        );
        assert_eq!(
            pull_transport(false, threshold + 1, threshold, true),
            Transport::Relay
        );
        assert_eq!(
            pull_transport(true, threshold, threshold, true),
            Transport::Relay
        );
        assert_eq!(
            pull_transport(true, threshold + 1, threshold, false),
            Transport::Relay
        );
    }

    #[test]
    fn only_an_external_pull_or_normal_query_leaves_our_cache() {
        assert!(may_consult_upstreams(QueryMode::Pull, true));
        assert!(may_consult_upstreams(QueryMode::Normal, true));
        assert!(!may_consult_upstreams(QueryMode::Push, true));
        assert!(!may_consult_upstreams(QueryMode::Pull, false));
        assert!(!may_consult_upstreams(QueryMode::Normal, false));
    }

    #[test]
    fn an_external_query_names_exactly_one_path() {
        assert!(external_arity_ok(false, 0) && external_arity_ok(false, 200));
        assert!(external_arity_ok(true, 1));
        assert!(!external_arity_ok(true, 0) && !external_arity_ok(true, 2));
    }
}
