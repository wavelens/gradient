/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
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
            self.want_mass_query, self.store_dir, self.priority
        )
    }
}

#[derive(Debug, Clone, Serialize)]
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
    pub file_size: u32,
    #[serde(rename = "NarHash")]
    pub nar_hash: String,
    #[serde(rename = "NarSize")]
    pub nar_size: u64,
    #[serde(rename = "References")]
    pub references: Vec<String>,
    #[serde(rename = "Sig")]
    pub sig: String,
    #[serde(rename = "Deriver")]
    pub deriver: Option<String>,
    #[serde(rename = "CA")]
    pub ca: Option<String>,
}

impl NixPathInfo {
    pub fn to_nix_string(&self) -> String {
        format!(
            "StorePath: {}\nURL: {}\nCompression: {}\nFileHash: {}\nFileSize: {}\nNarHash: {}\nNarSize: {}\nReferences: {}{}\nSig: {}{}\n",
            self.store_path,
            self.url,
            self.compression,
            self.file_hash,
            self.file_size,
            self.nar_hash,
            self.nar_size,
            self.references.join(" "),
            self.deriver
                .as_ref()
                .map(|deriver| format!("\nDeriver: {}", deriver))
                .unwrap_or_default(),
            self.sig,
            self.ca
                .as_ref()
                .map(|ca| format!("\nCA: {}", ca))
                .unwrap_or_default()
        )
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
            "WantMassQuery: true\nStoreDir: /nix/store\nPriority: 40"
        );
    }

    #[test]
    fn nix_cache_info_false_mass_query() {
        let info = NixCacheInfo {
            want_mass_query: false,
            store_dir: "/nix/store".to_string(),
            priority: 0,
        };
        assert!(info.to_nix_string().contains("WantMassQuery: false"));
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
            sig: "key:signature".into(),
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
    fn nix_path_info_references_space_joined() {
        let s = path_info().to_nix_string();
        assert!(s.contains("References: /nix/store/x-a /nix/store/y-b"));
    }

    #[test]
    fn nix_path_info_no_refs_empty_line() {
        let mut pi = path_info();
        pi.references = vec![];
        let s = pi.to_nix_string();
        assert!(s.contains("References: \n") || s.contains("References: "));
    }

    #[test]
    fn nix_path_info_deriver_appears_only_when_set() {
        let mut pi = path_info();
        assert!(!pi.to_nix_string().contains("Deriver:"));
        pi.deriver = Some("/nix/store/drv-path.drv".into());
        let s = pi.to_nix_string();
        assert!(s.contains("Deriver: /nix/store/drv-path.drv"));
    }

    #[test]
    fn nix_path_info_ca_appears_only_when_set() {
        let mut pi = path_info();
        assert!(!pi.to_nix_string().contains("CA:"));
        pi.ca = Some("fixed:r:sha256:deadbeef".into());
        let s = pi.to_nix_string();
        assert!(s.contains("CA: fixed:r:sha256:deadbeef"));
    }

    #[test]
    fn nix_path_info_deriver_placed_before_sig() {
        // The deriver field is inserted between References and Sig.
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
    fn build_output_path_deserializes() {
        let json = r#"{"id":"out","outPath":"/nix/store/abc-hello","signatures":["k:sig"]}"#;
        let bop: BuildOutputPath = serde_json::from_str(json).unwrap();
        assert_eq!(bop.id, "out");
        assert_eq!(bop.out_path, "/nix/store/abc-hello");
        assert_eq!(bop.signatures, vec!["k:sig".to_string()]);
    }
}
