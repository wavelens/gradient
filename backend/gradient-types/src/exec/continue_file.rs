/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use serde::Deserialize;

use crate::wildcard::Wildcard;

pub const MAX_CONTINUE_FILE_BYTES: usize = 65536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueFile {
    pub attributes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContinueFileError {
    #[error("gradient/continue.json: {0} bytes is above the limit of {MAX_CONTINUE_FILE_BYTES}")]
    TooLarge(usize),
    #[error("gradient/continue.json: expected {{\"attributes\": [..]}}: {0}")]
    NotJson(String),
    #[error("gradient/continue.json: \"{0}\" is not a concrete attribute path")]
    NotConcrete(String),
    #[error("gradient/continue.json: the attributes list is empty")]
    Empty,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContinueFile {
    attributes: Vec<String>,
}

impl ContinueFile {
    pub fn parse(bytes: &[u8]) -> Result<ContinueFile, ContinueFileError> {
        if bytes.len() > MAX_CONTINUE_FILE_BYTES {
            return Err(ContinueFileError::TooLarge(bytes.len()));
        }
        let raw: RawContinueFile =
            serde_json::from_slice(bytes).map_err(|e| ContinueFileError::NotJson(e.to_string()))?;
        if raw.attributes.is_empty() {
            return Err(ContinueFileError::Empty);
        }
        let mut seen = HashSet::new();
        let mut attributes = Vec::new();
        for attribute in raw.attributes {
            check_concrete(&attribute)?;
            if seen.insert(attribute.clone()) {
                attributes.push(attribute);
            }
        }
        Ok(ContinueFile { attributes })
    }
}

fn check_concrete(attribute: &str) -> Result<(), ContinueFileError> {
    let has_pattern_chars = attribute.contains(['*', '?', ',', '!']);
    if has_pattern_chars || attribute.parse::<Wildcard>().is_err() {
        return Err(ContinueFileError::NotConcrete(attribute.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_continue_file_lists_concrete_attribute_paths_once() {
        let file = ContinueFile::parse(
            br#"{"attributes":["packages.x86_64-linux.model","packages.x86_64-linux.model"]}"#,
        )
        .unwrap();
        assert_eq!(
            file.attributes,
            vec!["packages.x86_64-linux.model".to_owned()]
        );
        assert!(
            ContinueFile::parse(br#"{"attributes":["packages.*"]}"#)
                .unwrap_err()
                .to_string()
                .contains("packages.*")
        );
        assert!(matches!(
            ContinueFile::parse(br#"{"attributes":[]}"#),
            Err(ContinueFileError::Empty)
        ));
        assert!(matches!(
            ContinueFile::parse(b"[]"),
            Err(ContinueFileError::NotJson(_))
        ));
        assert!(matches!(
            ContinueFile::parse(&vec![b' '; MAX_CONTINUE_FILE_BYTES + 1]),
            Err(ContinueFileError::TooLarge(_))
        ));
    }
}
