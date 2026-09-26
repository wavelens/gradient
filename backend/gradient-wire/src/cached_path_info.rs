/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! View type for [`CachedPath`] that makes the cached/uncached distinction
//! explicit at the type level.
//!
//! The wire type [`CachedPath`] carries a `cached: bool` flag and a number of
//! fields that are only meaningful depending on that flag and the query mode.
//! [`CachedPathInfo`] is a zero-copy, lifetime-bound projection that lifts
//! the flag into an enum, making the two cases structurally distinct and
//! preventing callers from accidentally reading metadata fields on uncached
//! paths.

use crate::types::{CachedPath, PresignedMultipart};

/// Where an uncached path's NAR goes, as granted by a `CacheQuery {Push}`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UploadTarget<'a> {
    /// No presigner: chunked `NarPush` over the WebSocket.
    Relay,
    /// One presigned S3 PUT.
    Put(&'a str),
    /// A presigned S3 multipart upload.
    Multipart(&'a PresignedMultipart),
}

/// A zero-copy view of a [`CachedPath`] with the cached/uncached state
/// encoded in the enum variant.
///
/// Obtain via [`CachedPath::as_info`].
#[derive(Debug, Clone, PartialEq)]
pub enum CachedPathInfo<'a> {
    /// The path is **not** present in the Gradient cache.
    ///
    /// In [`QueryMode::Push`] contexts, `upload` says how the server wants
    /// the NAR delivered.
    Uncached {
        path: &'a str,
        upload: UploadTarget<'a>,
    },

    /// The path **is** present in the Gradient cache.
    ///
    /// In [`QueryMode::Pull`] contexts the metadata fields (`nar_hash`,
    /// `references`, `signatures`, `deriver`, `ca`) are populated so the
    /// caller can construct a `ValidPathInfo` and import the NAR into the
    /// local nix-daemon. In other query modes they may be `None`.
    Cached {
        path: &'a str,
        /// Presigned GET URL (S3 pull) or `None` for WebSocket `NarRequest`/`NarPush`.
        download_url: Option<&'a str>,
        file_size: Option<u64>,
        nar_size: Option<u64>,
        /// NAR hash in `sha256:<nix32>` format. Populated for Pull mode.
        nar_hash: Option<&'a str>,
        /// Store-path references. Populated for Pull mode.
        references: Option<&'a Vec<String>>,
        /// narinfo-format signatures. Populated for Pull mode.
        signatures: Option<&'a Vec<String>>,
        /// Deriver `.drv` path. Populated for Pull mode when known.
        deriver: Option<&'a str>,
        /// Content-address field. Populated for Pull mode when the path is CA.
        ca: Option<&'a str>,
    },
}

impl CachedPath {
    /// Return a zero-copy view of this [`CachedPath`] with the cached/uncached
    /// state encoded in the [`CachedPathInfo`] enum variant.
    pub fn as_info(&self) -> CachedPathInfo<'_> {
        if self.cached {
            CachedPathInfo::Cached {
                path: &self.path,
                download_url: self.url.as_deref(),
                file_size: self.file_size,
                nar_size: self.nar_size,
                nar_hash: self.nar_hash.as_deref(),
                references: self.references.as_ref(),
                signatures: self.signatures.as_ref(),
                deriver: self.deriver.as_deref(),
                ca: self.ca.as_deref(),
            }
        } else {
            CachedPathInfo::Uncached {
                path: &self.path,
                upload: match (&self.multipart, &self.url) {
                    (Some(multipart), _) => UploadTarget::Multipart(multipart),
                    (None, Some(url)) => UploadTarget::Put(url),
                    (None, None) => UploadTarget::Relay,
                },
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn uncached_path() -> CachedPath {
        CachedPath {
            path: "/nix/store/aaaa-pkg".into(),
            cached: false,
            file_size: None,
            nar_size: None,
            url: Some("https://s3.example.com/put-url".into()),
            multipart: None,
            nar_hash: None,
            file_hash: None,
            references: None,
            signatures: None,
            deriver: None,
            ca: None,
        }
    }

    fn cached_path() -> CachedPath {
        CachedPath {
            path: "/nix/store/bbbb-pkg".into(),
            cached: true,
            file_size: Some(1024),
            nar_size: Some(4096),
            url: Some("https://s3.example.com/get-url".into()),
            multipart: None,
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            file_hash: Some("sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf".into()),
            references: Some(vec!["/nix/store/cccc-dep".into()]),
            signatures: Some(vec!["cache.example.com-1:abc123==".into()]),
            deriver: Some("/nix/store/dddd-pkg.drv".into()),
            ca: None,
        }
    }

    #[test]
    fn as_info_uncached_has_upload_url() {
        let cp = uncached_path();
        match cp.as_info() {
            CachedPathInfo::Uncached { path, upload } => {
                assert_eq!(path, "/nix/store/aaaa-pkg");
                assert_eq!(upload, UploadTarget::Put("https://s3.example.com/put-url"));
            }
            other => panic!("expected Uncached, got {:?}", other),
        }
    }

    #[test]
    fn as_info_uncached_no_url() {
        let cp = CachedPath {
            url: None,
            ..uncached_path()
        };
        match cp.as_info() {
            CachedPathInfo::Uncached { upload, .. } => {
                assert_eq!(upload, UploadTarget::Relay);
            }
            other => panic!("expected Uncached, got {:?}", other),
        }
    }

    #[test]
    fn as_info_uncached_prefers_the_multipart_grant() {
        let grant = PresignedMultipart {
            upload_id: "up-1".into(),
            part_size: 64,
            part_urls: vec!["https://s3.example.com/part-1".into()],
        };
        let cp = CachedPath {
            url: None,
            multipart: Some(grant.clone()),
            ..uncached_path()
        };
        match cp.as_info() {
            CachedPathInfo::Uncached { upload, .. } => {
                assert_eq!(upload, UploadTarget::Multipart(&grant));
            }
            other => panic!("expected Uncached, got {:?}", other),
        }
    }

    #[test]
    fn as_info_cached_populates_metadata() {
        let cp = cached_path();
        match cp.as_info() {
            CachedPathInfo::Cached {
                path,
                download_url,
                file_size,
                nar_size,
                nar_hash,
                references,
                signatures,
                deriver,
                ca,
            } => {
                assert_eq!(path, "/nix/store/bbbb-pkg");
                assert_eq!(download_url, Some("https://s3.example.com/get-url"));
                assert_eq!(file_size, Some(1024));
                assert_eq!(nar_size, Some(4096));
                assert!(nar_hash.is_some());
                assert_eq!(references.map(|r| r.len()), Some(1));
                assert_eq!(signatures.map(|s| s.len()), Some(1));
                assert!(deriver.is_some());
                assert!(ca.is_none());
            }
            other => panic!("expected Cached, got {:?}", other),
        }
    }

    #[test]
    fn as_info_cached_no_url_uses_ws_transfer() {
        let cp = CachedPath {
            url: None,
            ..cached_path()
        };
        match cp.as_info() {
            CachedPathInfo::Cached { download_url, .. } => {
                assert_eq!(download_url, None);
            }
            other => panic!("expected Cached, got {:?}", other),
        }
    }
}
