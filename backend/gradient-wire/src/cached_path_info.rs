/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::CachedPath;

#[derive(Debug, Clone, PartialEq)]
pub enum CachedPathInfo<'a> {
    Uncached {
        path: &'a str,
    },

    /// The metadata fields are populated in [`QueryMode::Pull`] only. Other query modes can leave
    /// them `None`.
    Cached {
        path: &'a str,
        download_url: Option<&'a str>,
        file_size: Option<u64>,
        nar_size: Option<u64>,
        nar_hash: Option<&'a str>,
        references: Option<&'a Vec<String>>,
        signatures: Option<&'a Vec<String>>,
        deriver: Option<&'a str>,
        ca: Option<&'a str>,
    },
}

impl CachedPath {
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
            CachedPathInfo::Uncached { path: &self.path }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uncached_path() -> CachedPath {
        CachedPath {
            path: "/nix/store/aaaa-pkg".into(),
            cached: false,
            file_size: None,
            nar_size: None,
            url: None,
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
            nar_hash: Some("sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into()),
            file_hash: Some("sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf".into()),
            references: Some(vec!["/nix/store/cccc-dep".into()]),
            signatures: Some(vec!["cache.example.com-1:abc123==".into()]),
            deriver: Some("/nix/store/dddd-pkg.drv".into()),
            ca: None,
        }
    }

    #[test]
    fn as_info_uncached_names_the_path() {
        assert_eq!(
            uncached_path().as_info(),
            CachedPathInfo::Uncached {
                path: "/nix/store/aaaa-pkg"
            }
        );
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
}
