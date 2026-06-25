/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Upstream binary-cache narinfo lookup, shared by the cache-query handler
//! (worker pulls) and the eval-time substitutability probe (scheduler). Given a
//! set of upstream base URLs and a store-path hash, fetch and parse the
//! `<hash>.narinfo` into a [`CachedPath`] carrying the absolute NAR URL plus the
//! metadata needed to import the path.

use std::sync::Arc;

use gradient_types::proto::CachedPath;

/// Look up `<hash>.narinfo` across `upstream_urls`, returning the first hit as a
/// [`CachedPath`] with an absolute NAR `url`. `None` when no upstream serves it.
pub async fn lookup_upstream_narinfo(
    http: reqwest::Client,
    upstream_urls: Arc<Vec<String>>,
    hash: String,
    store_path: String,
) -> Option<CachedPath> {
    for base_url in upstream_urls.iter() {
        let narinfo_url = format!("{}/{}.narinfo", base_url.trim_end_matches('/'), &hash);
        let body = match http
            .get(&narinfo_url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => match r.text().await {
                Ok(b) => b,
                Err(_) => continue,
            },
            _ => continue,
        };
        if let Some(cp) = parse_upstream_narinfo(base_url, &store_path, &body) {
            return Some(cp);
        }
    }
    None
}

/// Parse a narinfo `body` into a [`CachedPath`]. The `URL:` field is resolved
/// against `base_url` into an absolute NAR URL; `None` if the body has no `URL:`.
pub fn parse_upstream_narinfo(
    base_url: &str,
    store_path: &str,
    body: &str,
) -> Option<CachedPath> {
    let mut nar_path: Option<&str> = None;
    let mut nar_hash: Option<String> = None;
    let mut file_hash: Option<String> = None;
    let mut nar_size: Option<u64> = None;
    let mut file_size: Option<u64> = None;
    let mut references: Option<Vec<String>> = None;
    let mut deriver: Option<String> = None;
    let mut ca: Option<String> = None;
    let mut sigs: Vec<String> = Vec::new();

    for line in body.lines() {
        if let Some(v) = line.strip_prefix("URL: ") {
            nar_path = Some(v.trim());
        } else if let Some(v) = line.strip_prefix("NarHash: ") {
            nar_hash = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("FileHash: ") {
            file_hash = Some(v.trim().to_owned());
        } else if let Some(v) = line.strip_prefix("NarSize: ") {
            nar_size = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("FileSize: ") {
            file_size = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("References: ") {
            references = Some(
                v.split_whitespace()
                    .map(|r| {
                        if r.starts_with("/nix/store/") {
                            r.to_owned()
                        } else {
                            format!("/nix/store/{}", r)
                        }
                    })
                    .collect(),
            );
        } else if let Some(v) = line.strip_prefix("Deriver: ") {
            let d = v.trim();
            if !d.is_empty() {
                deriver = Some(if d.starts_with("/nix/store/") {
                    d.to_owned()
                } else {
                    format!("/nix/store/{}", d)
                });
            }
        } else if let Some(v) = line.strip_prefix("CA: ") {
            let c = v.trim();
            if !c.is_empty() {
                ca = Some(c.to_owned());
            }
        } else if let Some(v) = line.strip_prefix("Sig: ") {
            sigs.push(v.trim().to_owned());
        }
    }

    let nar_path = nar_path?;
    let url = format!("{}/{}", base_url.trim_end_matches('/'), nar_path);

    Some(CachedPath {
        path: store_path.to_string(),
        cached: true,
        file_size,
        nar_size,
        url: Some(url),
        nar_hash,
        file_hash,
        references,
        signatures: if sigs.is_empty() { None } else { Some(sigs) },
        deriver,
        ca,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_upstream_narinfo_full_fields() {
        let body = "StorePath: /nix/store/6ak2iyrql4xlj0mpxcibqnzdlwl0vlwj-bzip2-0.6.1\n\
                    URL: nar/abc.nar.xz\n\
                    Compression: xz\n\
                    FileHash: sha256:124l7vc762nsgl8wmfgp1gm9vsl3gk0j6136nyv3ff7s7da11yvz\n\
                    FileSize: 35372\n\
                    NarHash: sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf\n\
                    NarSize: 102760\n\
                    References: aaaa-dep1 /nix/store/bbbb-dep2\n\
                    Deriver: vmc3d9j1qnwhqyxqkwzsnf3pv98shq18-bzip2-0.6.1.drv\n\
                    Sig: cache.nixos.org-1:a84Gyv6ieXj7HclpmXu/i+so=\n";
        let cp = parse_upstream_narinfo(
            "https://upstream.example/",
            "/nix/store/6ak2iyrql4xlj0mpxcibqnzdlwl0vlwj-bzip2-0.6.1",
            body,
        )
        .unwrap();
        assert!(cp.cached);
        assert_eq!(
            cp.url.as_deref(),
            Some("https://upstream.example/nar/abc.nar.xz")
        );
        assert_eq!(cp.nar_size, Some(102760));
        assert_eq!(cp.file_size, Some(35372));
        assert_eq!(
            cp.nar_hash.as_deref(),
            Some("sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf")
        );
        assert_eq!(
            cp.file_hash.as_deref(),
            Some("sha256:124l7vc762nsgl8wmfgp1gm9vsl3gk0j6136nyv3ff7s7da11yvz")
        );
        let refs = cp.references.unwrap();
        assert_eq!(refs.len(), 2);
        assert!(refs.contains(&"/nix/store/aaaa-dep1".to_string()));
        assert!(refs.contains(&"/nix/store/bbbb-dep2".to_string()));
        assert_eq!(
            cp.deriver.as_deref(),
            Some("/nix/store/vmc3d9j1qnwhqyxqkwzsnf3pv98shq18-bzip2-0.6.1.drv")
        );
        assert_eq!(cp.signatures.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn parse_upstream_narinfo_ca_field() {
        let body = "URL: nar/x.nar.xz\n\
                    NarHash: sha256:deadbeef\n\
                    NarSize: 1\n\
                    CA: fixed:sha256:0abc\n";
        let cp = parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).unwrap();
        assert_eq!(cp.ca.as_deref(), Some("fixed:sha256:0abc"));
    }

    #[test]
    fn parse_upstream_narinfo_empty_references_is_some_empty() {
        let body = "URL: nar/x.nar.xz\n\
                    NarHash: sha256:deadbeef\n\
                    NarSize: 1\n\
                    References: \n";
        let cp = parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).unwrap();
        assert_eq!(cp.references.as_deref(), Some(&[][..]));
    }

    #[test]
    fn parse_upstream_narinfo_requires_url() {
        let body = "NarHash: sha256:abc\nNarSize: 1\n";
        assert!(parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).is_none());
    }

    #[test]
    fn parse_upstream_narinfo_trims_base_url_trailing_slash() {
        let body = "URL: nar/x.nar\n";
        let cp = parse_upstream_narinfo("https://up.example/", "/nix/store/aa-x", body).unwrap();
        assert_eq!(cp.url.as_deref(), Some("https://up.example/nar/x.nar"));
    }

    #[test]
    fn parse_upstream_narinfo_ignores_unparseable_sizes() {
        let body = "URL: nar/x.nar\nNarSize: not-a-number\nFileSize: also-bad\n";
        let cp = parse_upstream_narinfo("https://up/", "/nix/store/aa-x", body).unwrap();
        assert!(cp.nar_size.is_none());
        assert!(cp.file_size.is_none());
    }
}
