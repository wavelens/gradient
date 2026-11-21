/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{OriginalStorePath, STORE_DIR};

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
    #[serde(rename = "FileHash", skip_serializing_if = "Option::is_none")]
    pub file_hash: Option<String>,
    #[serde(rename = "FileSize", skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NarInfoFieldValueError {
    #[error("Control character in narinfo field {key}")]
    FieldValueContainsControlCharacter { key: &'static str },
    #[error("Whitespace in the store path for narinfo field {key}")]
    StorePathValueContainsWhitespace { key: &'static str },
}

fn append_narinfo_line_rejecting_control_character_in_value(
    out: &mut String,
    key: &'static str,
    value: &str,
) -> Result<(), NarInfoFieldValueError> {
    if value.chars().any(char::is_control) {
        return Err(NarInfoFieldValueError::FieldValueContainsControlCharacter { key });
    }
    out.push_str(key);
    out.push_str(": ");
    out.push_str(value);
    out.push('\n');
    Ok(())
}

fn store_path_name_from_narinfo_reference_or_deriver_value_rejecting_whitespace<'value>(
    key: &'static str,
    value: &'value str,
) -> Result<&'value str, NarInfoFieldValueError> {
    if value.chars().any(char::is_control) {
        return Err(NarInfoFieldValueError::FieldValueContainsControlCharacter { key });
    }
    if value.chars().any(char::is_whitespace) {
        return Err(NarInfoFieldValueError::StorePathValueContainsWhitespace { key });
    }
    Ok(store_path_name(value))
}

impl NixPathInfo {
    pub fn to_nix_string(&self) -> Result<String, NarInfoFieldValueError> {
        let mut out = String::new();
        append_narinfo_line_rejecting_control_character_in_value(
            &mut out,
            "StorePath",
            &self.store_path,
        )?;
        append_narinfo_line_rejecting_control_character_in_value(&mut out, "URL", &self.url)?;
        append_narinfo_line_rejecting_control_character_in_value(
            &mut out,
            "Compression",
            &self.compression,
        )?;
        if let Some(file_hash) = &self.file_hash {
            append_narinfo_line_rejecting_control_character_in_value(
                &mut out, "FileHash", file_hash,
            )?;
        }
        if let Some(file_size) = self.file_size {
            append_narinfo_line_rejecting_control_character_in_value(
                &mut out,
                "FileSize",
                &file_size.to_string(),
            )?;
        }
        append_narinfo_line_rejecting_control_character_in_value(
            &mut out,
            "NarHash",
            &self.nar_hash,
        )?;
        append_narinfo_line_rejecting_control_character_in_value(
            &mut out,
            "NarSize",
            &self.nar_size.to_string(),
        )?;
        if !self.references.is_empty() {
            let refs = self
                .references
                .iter()
                .map(|r| {
                    store_path_name_from_narinfo_reference_or_deriver_value_rejecting_whitespace(
                        "References",
                        r,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(" ");
            append_narinfo_line_rejecting_control_character_in_value(
                &mut out,
                "References",
                &refs,
            )?;
        }
        if let Some(deriver) = self
            .deriver
            .as_deref()
            .map(|deriver| {
                store_path_name_from_narinfo_reference_or_deriver_value_rejecting_whitespace(
                    "Deriver", deriver,
                )
            })
            .transpose()?
            .filter(|d| !d.is_empty())
        {
            append_narinfo_line_rejecting_control_character_in_value(&mut out, "Deriver", deriver)?;
        }
        for signature in &self.signature_lines {
            append_narinfo_line_rejecting_control_character_in_value(&mut out, "Sig", signature)?;
        }
        if let Some(ca) = self.ca.as_deref().filter(|ca| !ca.is_empty()) {
            append_narinfo_line_rejecting_control_character_in_value(&mut out, "CA", ca)?;
        }
        Ok(out)
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NarInfoParseError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("line {line_number}: invalid value for {field}")]
    InvalidValue {
        line_number: usize,
        field: &'static str,
    },
    #[error("line {line_number}: {narinfo_line_error}")]
    InvalidNarInfoLine {
        line_number: usize,
        narinfo_line_error: NarInfoLineFormatError,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NarInfoLineFormatError {
    #[error("empty line")]
    EmptyNarinfoLine,
    #[error("control character")]
    NarinfoLineHasControlCharacter,
    #[error("not in the form ``Key: value``")]
    MissingColonSpaceBetweenKeyAndValue,
    #[error("key is not only ASCII letters and digits")]
    EmptyOrNonAlphanumericNarinfoLineKey,
    #[error("whitespace around the value of {0}")]
    ValueHasLeadingOrTrailingWhitespace(String),
    #[error("duplicate key {0}")]
    DuplicateKey(String),
}

/// All reads of a narinfo pass through this parser. Signature verification and re-signing never
/// see different values for the same key.
pub fn parse_narinfo_body(body: &str) -> Result<NixPathInfo, NarInfoParseError> {
    let mut fields: HashMap<&str, (usize, &str)> = HashMap::new();
    let mut signature_lines: Vec<String> = Vec::new();

    let body = body.strip_suffix('\n').unwrap_or(body);
    for (index, line) in body.split('\n').enumerate() {
        let line_number = index + 1;
        let refuse = |narinfo_line_error| NarInfoParseError::InvalidNarInfoLine {
            line_number,
            narinfo_line_error,
        };
        if line.is_empty() {
            return Err(refuse(NarInfoLineFormatError::EmptyNarinfoLine));
        }
        if line.chars().any(char::is_control) {
            return Err(refuse(
                NarInfoLineFormatError::NarinfoLineHasControlCharacter,
            ));
        }
        let Some((key, value)) = line
            .split_once(": ")
            .or_else(|| Some((line.strip_suffix(':')?, "")))
        else {
            return Err(refuse(
                NarInfoLineFormatError::MissingColonSpaceBetweenKeyAndValue,
            ));
        };
        if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(refuse(
                NarInfoLineFormatError::EmptyOrNonAlphanumericNarinfoLineKey,
            ));
        }
        if value.trim() != value {
            return Err(refuse(
                NarInfoLineFormatError::ValueHasLeadingOrTrailingWhitespace(key.to_string()),
            ));
        }
        if key == "Sig" {
            signature_lines.push(value.to_string());
        } else if fields.insert(key, (line_number, value)).is_some() {
            return Err(refuse(NarInfoLineFormatError::DuplicateKey(
                key.to_string(),
            )));
        }
    }

    let get = |key: &'static str| {
        fields
            .get(key)
            .copied()
            .ok_or(NarInfoParseError::MissingField(key))
    };
    let number = |key: &'static str, (line_number, value): (usize, &str)| {
        value
            .parse::<u64>()
            .map_err(|_| NarInfoParseError::InvalidValue {
                line_number,
                field: key,
            })
    };
    let optional = |key: &str| fields.get(key).map(|(_, value)| value.to_string());

    let store_path = get("StorePath")?.1.to_string();
    let url = get("URL")?.1.to_string();
    let compression = optional("Compression").unwrap_or_else(|| "bzip2".to_string());
    let file_hash = optional("FileHash");
    let file_size = fields
        .get("FileSize")
        .map(|&field| number("FileSize", field))
        .transpose()?;
    let nar_hash = get("NarHash")?.1.to_string();
    let nar_size = number("NarSize", get("NarSize")?)?;
    let store_path_name = |key: &'static str, (line_number, value): (usize, &str)| {
        let base_name = value
            .strip_prefix(STORE_DIR)
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(value);
        OriginalStorePath::parse_store_path_base_name(base_name)
            .map(|_| value.to_string())
            .map_err(|_| NarInfoParseError::InvalidValue {
                line_number,
                field: key,
            })
    };
    let references = fields
        .get("References")
        .map_or(Ok(Vec::new()), |&(line, value)| {
            value
                .split_whitespace()
                .map(|reference| store_path_name("References", (line, reference)))
                .collect()
        })?;
    let deriver = match fields.get("Deriver") {
        None | Some((_, "" | "unknown-deriver")) => None,
        Some(&field) => Some(store_path_name("Deriver", field)?),
    };
    let ca = optional("CA");
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

    const FIRST_REFERENCES_STORE_PATH_BASE_NAME: &str = "0123456789abcdfghijklmnpqrsvwxyz-a";
    const SECOND_REFERENCES_STORE_PATH_BASE_NAME: &str = "zyxwvsrqpnmlkjihgfdcba9876543210-b";

    fn path_info() -> NixPathInfo {
        NixPathInfo {
            store_path: "/nix/store/abc-hello".into(),
            url: "nar/aa/bbcc.nar.zst".into(),
            compression: "zstd".into(),
            file_hash: Some("sha256:fhash".into()),
            file_size: Some(1234),
            nar_hash: "sha256:nhash".into(),
            nar_size: 5678,
            references: vec![
                format!("/nix/store/{FIRST_REFERENCES_STORE_PATH_BASE_NAME}"),
                format!("/nix/store/{SECOND_REFERENCES_STORE_PATH_BASE_NAME}"),
            ],
            signature_lines: vec!["key:signature".into()],
            deriver: None,
            ca: None,
        }
    }

    #[test]
    fn nix_path_info_basic_fields() {
        let s = path_info().to_nix_string().unwrap();
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
        let s = path_info().to_nix_string().unwrap();
        assert!(s.contains(&format!("References: {FIRST_REFERENCES_STORE_PATH_BASE_NAME} {SECOND_REFERENCES_STORE_PATH_BASE_NAME}\n")));
        assert!(!s.contains(&format!(
            "/nix/store/{FIRST_REFERENCES_STORE_PATH_BASE_NAME}"
        )));
    }

    #[test]
    fn nix_path_info_no_refs_omits_line() {
        let mut pi = path_info();
        pi.references = vec![];
        let s = pi.to_nix_string().unwrap();
        assert!(
            !s.contains("References:"),
            "empty references must omit the line:\n{s}"
        );
    }

    #[test]
    fn nix_path_info_deriver_appears_only_when_set() {
        let mut pi = path_info();
        assert!(!pi.to_nix_string().unwrap().contains("Deriver:"));
        pi.deriver = Some("/nix/store/drv-path.drv".into());
        let s = pi.to_nix_string().unwrap();
        assert!(s.contains("Deriver: drv-path.drv"));
        assert!(!s.contains("Deriver: /nix/store/"));
    }

    #[test]
    fn nix_path_info_ca_appears_only_when_set() {
        let mut pi = path_info();
        assert!(
            !pi.to_nix_string().unwrap().contains("CA:"),
            "no ca must omit the CA line"
        );
        pi.ca = Some("text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944".into());
        let s = pi.to_nix_string().unwrap();
        assert!(
            s.contains("CA: text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944\n"),
            "content-addressed path must emit the CA line:\n{s}"
        );
    }

    #[test]
    fn nix_path_info_deriver_placed_before_sig() {
        let mut pi = path_info();
        pi.deriver = Some("/nix/store/drv-path.drv".into());
        let s = pi.to_nix_string().unwrap();
        let deriver_idx = s.find("Deriver:").unwrap();
        let sig_idx = s.find("Sig:").unwrap();
        assert!(deriver_idx < sig_idx, "Deriver must appear before Sig");
    }

    #[test]
    fn nix_path_info_ca_placed_after_sig() {
        let mut pi = path_info();
        pi.ca = Some("fixed:r:sha256:x".into());
        let s = pi.to_nix_string().unwrap();
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
        let s = pi.to_nix_string().unwrap();
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
    fn renders_narinfo_rejects_reference_or_deriver_with_space() {
        for info in [
            NixPathInfo {
                references: vec![format!(
                    "{FIRST_REFERENCES_STORE_PATH_BASE_NAME} {FIRST_REFERENCES_STORE_PATH_BASE_NAME}"
                )],
                ..path_info()
            },
            NixPathInfo {
                deriver: Some(format!(
                    "/nix/store/{FIRST_REFERENCES_STORE_PATH_BASE_NAME}.drv {FIRST_REFERENCES_STORE_PATH_BASE_NAME}.drv"
                )),
                ..path_info()
            },
        ] {
            assert!(matches!(
                info.to_nix_string(),
                Err(NarInfoFieldValueError::StorePathValueContainsWhitespace { .. })
            ));
        }
    }

    #[test]
    fn to_nix_string_fails_when_deriver_reference_content_address_or_signature_contains_line_feed()
    {
        let injected = "x\nInjected: value";
        for info in [
            NixPathInfo {
                deriver: Some(format!(
                    "/nix/store/{FIRST_REFERENCES_STORE_PATH_BASE_NAME}.drv{injected}"
                )),
                ..path_info()
            },
            NixPathInfo {
                references: vec![format!(
                    "/nix/store/{FIRST_REFERENCES_STORE_PATH_BASE_NAME}{injected}"
                )],
                ..path_info()
            },
            NixPathInfo {
                ca: Some(format!("fixed:r:sha256:{injected}")),
                ..path_info()
            },
            NixPathInfo {
                signature_lines: vec![format!("key:{injected}")],
                ..path_info()
            },
        ] {
            assert!(matches!(
                info.to_nix_string(),
                Err(NarInfoFieldValueError::FieldValueContainsControlCharacter { .. })
            ));
        }
    }

    #[test]
    fn parse_narinfo_body_roundtrip() {
        let original = NixPathInfo {
            store_path: "/nix/store/abc-foo".into(),
            url: "nar/xyz.nar.zst".into(),
            compression: "zstd".into(),
            file_hash: Some("sha256:aaaa".into()),
            file_size: Some(1234),
            nar_hash: "sha256:bbbb".into(),
            nar_size: 5678,
            references: vec![
                FIRST_REFERENCES_STORE_PATH_BASE_NAME.into(),
                SECOND_REFERENCES_STORE_PATH_BASE_NAME.into(),
            ],
            signature_lines: vec![
                "cache.example:abcdef".into(),
                "cache.example:retired".into(),
            ],
            deriver: Some(format!("{FIRST_REFERENCES_STORE_PATH_BASE_NAME}.drv")),
            ca: None,
        };
        let text = original.to_nix_string().unwrap();
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
            file_size: Some(5_000_000_000),
            ..path_info()
        };
        let text = large.to_nix_string().unwrap();

        assert!(text.contains("FileSize: 5000000000\n"));
        let parsed = parse_narinfo_body(&text).expect("parse must succeed");
        assert_eq!(parsed.file_size, Some(5_000_000_000));
    }

    #[test]
    fn test_parse_narinfo_without_file_hash_size_and_compression_defaults_to_bzip2() {
        let body = "StorePath: /nix/store/abc-hello\nURL: nar/abc.nar\nNarHash: sha256:nhash\n\
NarSize: 5678\nSig: key:signature\n";

        let parsed = parse_narinfo_body(body).expect("These keys are optional for Nix.");

        assert_eq!(parsed.compression, "bzip2");
        assert_eq!(parsed.file_hash, None);
        assert_eq!(parsed.file_size, None);
        assert!(!parsed.to_nix_string().unwrap().contains("File"));
    }

    #[test]
    fn parse_narinfo_body_missing_required_field() {
        let body = "Compression: zstd\n";
        assert!(parse_narinfo_body(body).is_err());
    }

    const HELLO_NARINFO_TEXT: &str = "StorePath: /nix/store/0vk2y2ax4sy7yxbg0rb5hpvb3dp6zl8i-hello-2.12.1\n\
URL: nar/1njv6dxi9xm2blq6gq7y7kyij3q3nm4kpbgw4nfr2ldwqxvzm2c3.nar.xz\n\
Compression: xz\n\
FileHash: sha256:1njv6dxi9xm2blq6gq7y7kyij3q3nm4kpbgw4nfr2ldwqxvzm2c3\n\
FileSize: 50088\n\
NarHash: sha256:0yzhigwjl6bws649vcs2asa4lbs8hg93hyix187gc7s7a74w5h80\n\
NarSize: 226552\n\
References: 0vk2y2ax4sy7yxbg0rb5hpvb3dp6zl8i-hello-2.12.1 qn3ggz5sf3hkjs2c797xf7nan3amdxmp-glibc-2.38-27\n\
Deriver: 9wr0vsd5qbbxdj6v3ijz9qbcdmfdnd0r-hello-2.12.1.drv\n\
Sig: cache.nixos.org-1:5WvGYRAXbJyi1fmR3jJ3GsYQ2uPcxUhb5J0xVKOrSRXm6d1MKHpWtK3lpfP+n+X0dyvyy5PJ+sxBX0bXAzMrCw==\n";

    fn hello_narinfo_with_key_line_replaced(key: &str, replacement: &str) -> String {
        HELLO_NARINFO_TEXT
            .lines()
            .map(|line| {
                if line.starts_with(&format!("{key}: ")) {
                    replacement.to_string()
                } else {
                    line.to_string()
                }
            })
            .map(|line| line + "\n")
            .collect()
    }

    #[test]
    fn a_real_cache_nixos_org_narinfo_parses() {
        let parsed = parse_narinfo_body(HELLO_NARINFO_TEXT).expect("parse must succeed");
        assert_eq!(
            parsed.store_path,
            "/nix/store/0vk2y2ax4sy7yxbg0rb5hpvb3dp6zl8i-hello-2.12.1"
        );
        assert_eq!(parsed.nar_size, 226552);
        assert_eq!(parsed.references.len(), 2);
        assert_eq!(parsed.signature_lines.len(), 1);
    }

    #[test]
    fn a_narinfo_with_an_empty_references_value_parses() {
        let body = hello_narinfo_with_key_line_replaced("References", "References: ");
        let parsed = parse_narinfo_body(&body).expect("parse must succeed");
        assert!(parsed.references.is_empty());
    }

    #[test]
    fn references_line_with_no_value_parses_to_empty_references() {
        let body = hello_narinfo_with_key_line_replaced("References", "References:");
        let parsed = parse_narinfo_body(&body).expect("`Key:` is an empty value");
        assert!(parsed.references.is_empty());
    }

    #[test]
    fn references_with_comma_between_store_paths_is_refused() {
        let body = hello_narinfo_with_key_line_replaced(
            "References",
            &format!(
                "References: {FIRST_REFERENCES_STORE_PATH_BASE_NAME},/nix/store/{SECOND_REFERENCES_STORE_PATH_BASE_NAME}"
            ),
        );
        let error = parse_narinfo_body(&body).expect_err("a comma is not part of a store path");
        assert!(error.to_string().contains("References"), "{error}");
    }

    #[test]
    fn absolute_store_path_reference_with_double_slash_is_rejected() {
        let accepted = hello_narinfo_with_key_line_replaced(
            "References",
            &format!("References: /nix/store/{FIRST_REFERENCES_STORE_PATH_BASE_NAME}"),
        );
        assert!(parse_narinfo_body(&accepted).is_ok());
        let refused = hello_narinfo_with_key_line_replaced(
            "References",
            &format!("References: /nix/store//{FIRST_REFERENCES_STORE_PATH_BASE_NAME}"),
        );
        assert!(parse_narinfo_body(&refused).is_err());
    }

    #[test]
    fn rejects_deriver_that_is_not_a_valid_store_path_name() {
        let body = hello_narinfo_with_key_line_replaced(
            "Deriver",
            &format!(
                "Deriver: {FIRST_REFERENCES_STORE_PATH_BASE_NAME}.drv,/nix/store/{SECOND_REFERENCES_STORE_PATH_BASE_NAME}"
            ),
        );
        let error = parse_narinfo_body(&body).expect_err("deriver isn't a name of a store path");
        assert!(error.to_string().contains("Deriver"), "{error}");
    }

    #[test]
    fn unknown_deriver_parses_as_no_deriver() {
        let body = hello_narinfo_with_key_line_replaced("Deriver", "Deriver: unknown-deriver");
        let parsed = parse_narinfo_body(&body).expect("Nix can write `unknown-deriver`.");
        assert_eq!(parsed.deriver, None);
    }

    #[test]
    fn rejects_narinfo_text_starting_with_byte_order_mark_on_line_1() {
        let body = format!("\u{feff}{HELLO_NARINFO_TEXT}");
        let error = parse_narinfo_body(&body).expect_err("Reject a byte order mark");
        assert!(error.to_string().contains("line 1"), "{error}");
    }

    #[test]
    fn rejects_narinfo_with_empty_last_line() {
        let body = format!("{HELLO_NARINFO_TEXT}\n");
        assert!(parse_narinfo_body(&body).is_err());
    }

    #[test]
    fn a_duplicate_nar_hash_line_is_refused_with_its_line_number() {
        let body = format!(
            "{HELLO_NARINFO_TEXT}NarHash: sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf\n"
        );
        let error = parse_narinfo_body(&body).expect_err("a second NarHash must be refused");
        let message = error.to_string();
        assert!(message.contains("line 11"), "{message}");
        assert!(message.contains("NarHash"), "{message}");
        assert!(!message.contains("1bnnhb0pfx49"), "{message}");
    }

    #[test]
    fn a_duplicate_store_path_line_is_refused() {
        let body = format!("StorePath: /nix/store/aaaa-other\n{HELLO_NARINFO_TEXT}");
        let error = parse_narinfo_body(&body).expect_err("a second StorePath must be refused");
        let message = error.to_string();
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("StorePath"), "{message}");
        assert!(!message.contains("aaaa-other"), "{message}");
    }

    #[test]
    fn a_key_without_a_space_after_the_colon_is_refused() {
        let body = hello_narinfo_with_key_line_replaced(
            "StorePath",
            "StorePath:/nix/store/0vk2y2ax4sy7yxbg0rb5hpvb3dp6zl8i-hello-2.12.1",
        );
        let error = parse_narinfo_body(&body).expect_err("`Key:value` must be refused");
        assert!(error.to_string().contains("line 1"), "{error}");
    }

    #[test]
    fn a_line_without_a_colon_is_refused() {
        let body = format!("{HELLO_NARINFO_TEXT}garbage\n");
        let error = parse_narinfo_body(&body).expect_err("a line without a key must be refused");
        assert!(error.to_string().contains("line 11"), "{error}");
    }

    #[test]
    fn a_carriage_return_is_refused() {
        let body = HELLO_NARINFO_TEXT.replace('\n', "\r\n");
        assert!(parse_narinfo_body(&body).is_err());
    }

    #[test]
    fn an_empty_line_is_refused() {
        let body = HELLO_NARINFO_TEXT.replacen('\n', "\n\n", 1);
        let error = parse_narinfo_body(&body).expect_err("an empty line must be refused");
        assert!(error.to_string().contains("line 2"), "{error}");
    }

    #[test]
    fn two_sig_lines_are_kept_in_order() {
        let body = format!("{HELLO_NARINFO_TEXT}Sig: gradient.example-1:second\n");
        let parsed = parse_narinfo_body(&body).expect("several Sig lines are valid");
        assert_eq!(parsed.signature_lines.len(), 2);
        assert!(parsed.signature_lines[0].starts_with("cache.nixos.org-1:"));
        assert_eq!(parsed.signature_lines[1], "gradient.example-1:second");
    }

    #[test]
    fn a_narinfo_rendered_by_gradient_parses_back_to_the_same_text() {
        for info in [
            path_info(),
            NixPathInfo {
                references: Vec::new(),
                deriver: None,
                ca: Some(
                    "fixed:r:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf".into(),
                ),
                signature_lines: vec!["a-1:one".into(), "b-1:two".into()],
                ..path_info()
            },
        ] {
            let text = info.to_nix_string().unwrap();
            let parsed = parse_narinfo_body(&text).expect("a rendered narinfo must parse");
            assert_eq!(parsed.to_nix_string().unwrap(), text);
        }
    }
}
