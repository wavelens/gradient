/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct NixCacheInfo {
    #[serde(rename = "WantMassQuery")]
    pub want_mass_query: bool,
    #[serde(rename = "StoreDir")]
    pub store_dir: String,
    #[serde(rename = "Priority")]
    pub priority: i32,
}

impl NixCacheInfo {
    pub fn to_nix_string(&self) -> String {
        format!(
            "WantMassQuery: {}\nStoreDir: {}\nPriority: {}",
            u8::from(self.want_mass_query),
            self.store_dir,
            self.priority
        )
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct NixPathInfo {
    #[serde(rename = "StorePath")]
    pub store_path: String,
    #[serde(rename = "URL")]
    pub url: String,
    #[serde(rename = "Compression")]
    pub compression: String,
    #[serde(rename = "FileHash")]
    pub file_hash: String,
    #[serde(rename = "FileSize")]
    pub file_size: u64,
    #[serde(rename = "NarHash")]
    pub nar_hash: String,
    #[serde(rename = "NarSize")]
    pub nar_size: u64,
    #[serde(rename = "References")]
    pub references: Vec<String>,
    #[serde(rename = "Sig")]
    pub signature_lines: Vec<String>,
    #[serde(rename = "Deriver")]
    pub deriver: Option<String>,
    #[serde(rename = "CA")]
    pub ca: Option<String>,
}

/// Narinfo `References` and `Deriver` are basenames, never absolute paths. Values already in
/// name form are passing through unchanged.
fn store_path_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl NixPathInfo {
    pub fn to_nix_string(&self) -> String {
        let mut out = format!(
            "StorePath: {}\nURL: {}\nCompression: {}\nFileHash: {}\nFileSize: {}\nNarHash: {}\nNarSize: {}\n",
            self.store_path,
            self.url,
            self.compression,
            self.file_hash,
            self.file_size,
            self.nar_hash,
            self.nar_size,
        );
        if !self.references.is_empty() {
            let refs = self
                .references
                .iter()
                .map(|r| store_path_name(r))
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("References: {}\n", refs));
        }
        if let Some(deriver) = self
            .deriver
            .as_deref()
            .map(store_path_name)
            .filter(|d| !d.is_empty())
        {
            out.push_str(&format!("Deriver: {}\n", deriver));
        }
        for signature in &self.signature_lines {
            out.push_str(&format!("Sig: {signature}\n"));
        }
        if let Some(ca) = self.ca.as_deref().filter(|ca| !ca.is_empty()) {
            out.push_str(&format!("CA: {}\n", ca));
        }
        out
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildOutputPath {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "outPath")]
    pub out_path: String,
    #[serde(rename = "signatures")]
    pub signatures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GradientCacheInfo {
    #[serde(rename = "GradientVersion")]
    pub gradient_version: String,
    #[serde(rename = "GradientUrl")]
    pub gradient_url: String,
}

impl GradientCacheInfo {
    pub fn to_nix_string(&self) -> String {
        format!(
            "GradientVersion: {}\nGradientUrl: {}\n",
            self.gradient_version, self.gradient_url
        )
    }
}

#[derive(Debug, Error)]
pub enum NarInfoParseError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid value for {field}: {value}")]
    InvalidValue { field: &'static str, value: String },
}

pub fn parse_narinfo_body(body: &str) -> Result<NixPathInfo, NarInfoParseError> {
    let mut kv: HashMap<&str, &str> = HashMap::new();
    let mut signature_lines: Vec<String> = Vec::new();

    for line in body.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        if k == "Sig" {
            signature_lines.push(v.to_string());
        } else {
            kv.insert(k, v);
        }
    }

    let get = |k: &'static str| kv.get(k).copied().ok_or(NarInfoParseError::MissingField(k));

    let store_path = get("StorePath")?.to_string();
    let url = get("URL")?.to_string();
    let compression = get("Compression")?.to_string();
    let file_hash = get("FileHash")?.to_string();
    let file_size_raw = get("FileSize")?;
    let file_size: u64 = file_size_raw
        .parse()
        .map_err(|_| NarInfoParseError::InvalidValue {
            field: "FileSize",
            value: file_size_raw.to_string(),
        })?;
    let nar_hash = get("NarHash")?.to_string();
    let nar_size_raw = get("NarSize")?;
    let nar_size: u64 = nar_size_raw
        .parse()
        .map_err(|_| NarInfoParseError::InvalidValue {
            field: "NarSize",
            value: nar_size_raw.to_string(),
        })?;
    let references: Vec<String> = kv
        .get("References")
        .copied()
        .unwrap_or("")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let deriver = kv.get("Deriver").map(|s| s.to_string());
    let ca = kv.get("CA").map(|s| s.to_string());
    if signature_lines.is_empty() {
        return Err(NarInfoParseError::MissingField("Sig"));
    }

    Ok(NixPathInfo {
        store_path,
        url,
        compression,
        file_hash,
        file_size,
        nar_hash,
        nar_size,
        references,
        signature_lines,
        deriver,
        ca,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nix_cache_info_to_nix_string() {
        let info = NixCacheInfo {
            want_mass_query: true,
            store_dir: "/nix/store".to_string(),
            priority: 40,
        };
        assert_eq!(
            info.to_nix_string(),
            "WantMassQuery: 1\nStoreDir: /nix/store\nPriority: 40"
        );
    }

    #[test]
    fn nix_cache_info_false_mass_query() {
        let info = NixCacheInfo {
            want_mass_query: false,
            store_dir: "/nix/store".to_string(),
            priority: 0,
        };
        assert!(info.to_nix_string().contains("WantMassQuery: 0"));
        assert!(info.to_nix_string().contains("Priority: 0"));
    }

    fn path_info() -> NixPathInfo {
        NixPathInfo {
            store_path: "/nix/store/abc-hello".into(),
            url: "nar/aa/bbcc.nar.zst".into(),
            compression: "zstd".into(),
            file_hash: "sha256:fhash".into(),
            file_size: 1234,
            nar_hash: "sha256:nhash".into(),
            nar_size: 5678,
            references: vec!["/nix/store/x-a".into(), "/nix/store/y-b".into()],
            signature_lines: vec!["key:signature".into()],
            deriver: None,
            ca: None,
        }
    }

    #[test]
    fn nix_path_info_basic_fields() {
        let s = path_info().to_nix_string();
        assert!(s.contains("StorePath: /nix/store/abc-hello\n"));
        assert!(s.contains("URL: nar/aa/bbcc.nar.zst\n"));
        assert!(s.contains("Compression: zstd\n"));
        assert!(s.contains("FileHash: sha256:fhash\n"));
        assert!(s.contains("FileSize: 1234\n"));
        assert!(s.contains("NarHash: sha256:nhash\n"));
        assert!(s.contains("NarSize: 5678\n"));
        assert!(s.contains("Sig: key:signature"));
    }

    #[test]
    fn nix_path_info_references_are_basenames() {
        let s = path_info().to_nix_string();
        assert!(s.contains("References: x-a y-b"));
        assert!(!s.contains("/nix/store/x-a"));
    }

    #[test]
    fn nix_path_info_no_refs_omits_line() {
        let mut pi = path_info();
        pi.references = vec![];
        let s = pi.to_nix_string();
        assert!(
            !s.contains("References:"),
            "empty references must omit the line:\n{s}"
        );
    }

    #[test]
    fn nix_path_info_deriver_appears_only_when_set() {
        let mut pi = path_info();
        assert!(!pi.to_nix_string().contains("Deriver:"));
        pi.deriver = Some("/nix/store/drv-path.drv".into());
        let s = pi.to_nix_string();
        assert!(s.contains("Deriver: drv-path.drv"));
        assert!(!s.contains("Deriver: /nix/store/"));
    }

    #[test]
    fn nix_path_info_ca_appears_only_when_set() {
        let mut pi = path_info();
        assert!(
            !pi.to_nix_string().contains("CA:"),
            "no ca must omit the CA line"
        );
        pi.ca = Some("text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944".into());
        let s = pi.to_nix_string();
        assert!(
            s.contains("CA: text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944\n"),
            "content-addressed path must emit the CA line:\n{s}"
        );
    }

    #[test]
    fn nix_path_info_deriver_placed_before_sig() {
        let mut pi = path_info();
        pi.deriver = Some("/nix/store/drv-path.drv".into());
        let s = pi.to_nix_string();
        let deriver_idx = s.find("Deriver:").unwrap();
        let sig_idx = s.find("Sig:").unwrap();
        assert!(deriver_idx < sig_idx, "Deriver must appear before Sig");
    }

    #[test]
    fn nix_path_info_ca_placed_after_sig() {
        let mut pi = path_info();
        pi.ca = Some("fixed:r:sha256:x".into());
        let s = pi.to_nix_string();
        let sig_idx = s.find("Sig:").unwrap();
        let ca_idx = s.find("CA:").unwrap();
        assert!(sig_idx < ca_idx, "CA must appear after Sig");
    }

    #[test]
    fn rendered_narinfo_with_empty_deriver_and_ca_omits_deriver_and_ca_lines_has_nonempty_key_value_lines_and_one_trailing_newline()
     {
        let mut pi = path_info();
        pi.deriver = Some(String::new());
        pi.ca = Some(String::new());
        let s = pi.to_nix_string();
        assert!(
            !s.contains("Deriver:"),
            "Always omit the Deriver line for an empty deriver.\n{s}"
        );
        assert!(!s.contains("CA:"), "Omit the CA line for an empty CA.\n{s}");
        for line in s.lines() {
            assert!(
                line.split_once(": ")
                    .is_some_and(|(key, value)| !key.is_empty() && !value.trim().is_empty()),
                "All lines need a `Key: value` with a non-empty value, but got {line:?} in:\n{s}"
            );
        }
        assert!(
            s.ends_with('\n') && !s.ends_with("\n\n"),
            "End the body with exactly one newline. The body is {s:?}"
        );
    }

    #[test]
    fn parse_narinfo_body_roundtrip() {
        let original = NixPathInfo {
            store_path: "/nix/store/abc-foo".into(),
            url: "nar/xyz.nar.zst".into(),
            compression: "zstd".into(),
            file_hash: "sha256:aaaa".into(),
            file_size: 1234,
            nar_hash: "sha256:bbbb".into(),
            nar_size: 5678,
            references: vec!["dep1-foo".into(), "dep2-bar".into()],
            signature_lines: vec![
                "cache.example:abcdef".into(),
                "cache.example:retired".into(),
            ],
            deriver: Some("zzz-foo.drv".into()),
            ca: None,
        };
        let text = original.to_nix_string();
        let parsed = parse_narinfo_body(&text).expect("parse must succeed");
        assert_eq!(parsed.store_path, original.store_path);
        assert_eq!(parsed.url, original.url);
        assert_eq!(parsed.compression, original.compression);
        assert_eq!(parsed.file_hash, original.file_hash);
        assert_eq!(parsed.file_size, original.file_size);
        assert_eq!(parsed.nar_hash, original.nar_hash);
        assert_eq!(parsed.nar_size, original.nar_size);
        assert_eq!(parsed.references, original.references);
        assert_eq!(parsed.signature_lines, original.signature_lines);
        assert_eq!(parsed.deriver, original.deriver);
        assert_eq!(parsed.ca, original.ca);
    }

    #[test]
    fn a_nar_over_4_gib_keeps_its_file_size() {
        let large = NixPathInfo {
            file_size: 5_000_000_000,
            ..path_info()
        };
        let text = large.to_nix_string();

        assert!(text.contains("FileSize: 5000000000\n"));
        let parsed = parse_narinfo_body(&text).expect("parse must succeed");
        assert_eq!(parsed.file_size, 5_000_000_000);
    }

    #[test]
    fn parse_narinfo_body_missing_required_field() {
        let body = "Compression: zstd\n";
        assert!(parse_narinfo_body(body).is_err());
    }
}
