/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Debug, Clone, PartialEq)]
pub struct Narinfo {
    pub store_path: String,
    pub url: Option<String>,
    pub file_hash: String,
    pub file_size: i64,
    pub nar_hash: String,
    pub nar_size: i64,
    pub references: Vec<String>,
    pub deriver: Option<String>,
}

impl Narinfo {
    pub fn parse(text: &str) -> Result<Narinfo, String> {
        let mut store_path = None;
        let mut url = None;
        let mut file_hash = None;
        let mut file_size = None;
        let mut nar_hash = None;
        let mut nar_size = None;
        let mut references = Vec::new();
        let mut deriver = None;

        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else { continue };
            let value = value.trim();
            match key.trim() {
                "StorePath" => store_path = Some(value.to_string()),
                "URL" => url = Some(value.to_string()),
                "FileHash" => file_hash = Some(value.to_string()),
                "FileSize" => file_size = Some(value.parse::<i64>().map_err(|_| "bad FileSize")?),
                "NarHash" => nar_hash = Some(value.to_string()),
                "NarSize" => nar_size = Some(value.parse::<i64>().map_err(|_| "bad NarSize")?),
                "References" => {
                    references = value.split_whitespace().map(str::to_string).collect();
                }
                "Deriver" if !value.is_empty() && value != "unknown-deriver" => {
                    deriver = Some(value.to_string());
                }
                _ => {}
            }
        }

        Ok(Narinfo {
            store_path: store_path.ok_or("missing StorePath")?,
            url,
            file_hash: file_hash.ok_or("missing FileHash")?,
            file_size: file_size.ok_or("missing FileSize")?,
            nar_hash: nar_hash.ok_or("missing NarHash")?,
            nar_size: nar_size.ok_or("missing NarSize")?,
            references,
            deriver,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12\n\
URL: nar/abc.nar.xz\n\
Compression: xz\n\
FileHash: sha256:1f\n\
FileSize: 42\n\
NarHash: sha256:2e\n\
NarSize: 99\n\
References: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-libc cccccccccccccccccccccccccccccccc-zlib\n\
Deriver: dddddddddddddddddddddddddddddddd-hello-2.12.drv\n";

    #[test]
    fn parses_full_narinfo() {
        let ni = Narinfo::parse(SAMPLE).expect("parse");
        assert_eq!(ni.store_path, "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12");
        assert_eq!(ni.file_size, 42);
        assert_eq!(ni.nar_size, 99);
        assert_eq!(ni.references.len(), 2);
        assert_eq!(ni.deriver.as_deref(), Some("dddddddddddddddddddddddddddddddd-hello-2.12.drv"));
    }

    #[test]
    fn missing_required_field_errors() {
        assert!(Narinfo::parse("URL: x\n").is_err());
    }

    #[test]
    fn empty_references_ok() {
        let txt = "StorePath: /nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-x\nFileHash: sha256:a\nFileSize: 1\nNarHash: sha256:b\nNarSize: 1\nReferences: \n";
        let ni = Narinfo::parse(txt).unwrap();
        assert!(ni.references.is_empty());
    }
}
