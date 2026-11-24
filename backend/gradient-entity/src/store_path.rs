/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const STORE_DIR: &str = "/nix/store";

pub const NIX32_ALPHABET: &str = "0123456789abcdfghijklmnpqrsvwxyz";
pub(crate) const STORE_HASH_LENGTH: usize = 32;
const STORE_PATH_NAME_MAX_LENGTH: usize = 211;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StoreHash(String);

impl StoreHash {
    pub fn parse(hash: &str) -> Result<Self, StorePathError> {
        let is_nix32 =
            hash.len() == STORE_HASH_LENGTH && hash.chars().all(|c| NIX32_ALPHABET.contains(c));

        if !is_nix32 {
            return Err(StorePathError::Malformed(hash.to_owned()));
        }

        Ok(Self(hash.to_owned()))
    }

    pub(crate) fn stored(hash: String) -> Self {
        Self(hash)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StoreHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn split_base(input: &str) -> Result<(&str, &str), StorePathError> {
    let base = input
        .strip_prefix(STORE_DIR)
        .map(|rest| rest.trim_start_matches('/'))
        .unwrap_or(input);

    match base.split_once('-') {
        Some((hash, name)) if !hash.is_empty() && !name.is_empty() => Ok((hash, name)),
        _ => Err(StorePathError::Malformed(input.to_owned())),
    }
}

fn validate_store_path_base_name(base_name: &str) -> Result<(StoreHash, &str), StorePathError> {
    let malformed = || StorePathError::Malformed(base_name.to_owned());
    let (hash, name) = base_name.split_once('-').ok_or_else(malformed)?;
    let is_dot_or_parent_directory_name =
        matches!(name, "." | "..") || name.starts_with(".-") || name.starts_with("..-");
    let store_path_name_follows_nix_rules = !name.is_empty()
        && name.len() <= STORE_PATH_NAME_MAX_LENGTH
        && !is_dot_or_parent_directory_name
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-._?=".contains(&byte));
    if !store_path_name_follows_nix_rules {
        return Err(malformed());
    }
    let hash = StoreHash::parse(hash).map_err(|_| malformed())?;

    Ok((hash, name))
}

fn strip_nix_store_prefix(input: &str) -> &str {
    input
        .strip_prefix(STORE_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(input)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OriginalStorePath {
    hash: StoreHash,
    name: String,
}

impl OriginalStorePath {
    pub(crate) fn from_hash(hash: StoreHash, name: &str) -> Self {
        Self {
            hash,
            name: name.to_owned(),
        }
    }

    pub fn parse(input: &str) -> Result<Self, StorePathError> {
        let (hash, name) = validate_store_path_base_name(strip_nix_store_prefix(input))
            .map_err(|_| StorePathError::Malformed(input.to_owned()))?;
        Ok(Self::from_hash(hash, name))
    }

    /// Accept only ``<hash>-<name>`` without the store directory, with the characters that Nix
    /// allows in names of store paths.
    pub fn parse_store_path_base_name(input: &str) -> Result<Self, StorePathError> {
        let (hash, name) = validate_store_path_base_name(input)?;
        Ok(Self::from_hash(hash, name))
    }

    pub fn hash(&self) -> &StoreHash {
        &self.hash
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn base(&self) -> String {
        format!("{}-{}", self.hash, self.name)
    }

    pub fn full(&self) -> String {
        format!("{}/{}", STORE_DIR, self.base())
    }
}

impl fmt::Display for OriginalStorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.hash, self.name)
    }
}

// Stored without the `/nix/store/` prefix; `name` keeps a trailing `.drv` for derivations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StorePath {
    hash: StoreHash,
    name: String,
}

impl StorePath {
    pub fn from_hash_and_name_without_validation(
        hash: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            hash: StoreHash::stored(hash.into()),
            name: name.into(),
        }
    }

    pub(crate) fn from_hash(hash: StoreHash, name: &str) -> Self {
        Self {
            hash,
            name: name.to_owned(),
        }
    }

    pub(crate) fn store_hash(&self) -> &StoreHash {
        &self.hash
    }

    pub fn parse_stored(input: &str) -> Result<Self, StorePathError> {
        let (hash, name) = validate_store_path_base_name(strip_nix_store_prefix(input))
            .map_err(|_| StorePathError::Malformed(input.to_owned()))?;
        Ok(Self::from_hash(hash, name))
    }

    fn store_path_from_database_column_text(text: &str) -> Result<Self, StorePathError> {
        let (hash, name) = split_base(text)?;
        Ok(Self::from_hash_and_name_without_validation(hash, name))
    }

    pub fn hash(&self) -> &str {
        self.hash.as_str()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_derivation(&self) -> bool {
        self.name.ends_with(".drv")
    }

    pub fn base(&self) -> String {
        format!("{}-{}", self.hash, self.name)
    }

    pub fn full(&self) -> String {
        format!("{}/{}-{}", STORE_DIR, self.hash, self.name)
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.hash, self.name)
    }
}

impl FromStr for StorePath {
    type Err = StorePathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_stored(s)
    }
}

impl Serialize for StorePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.base())
    }
}

impl<'de> Deserialize<'de> for StorePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        StorePath::parse_stored(&raw).map_err(serde::de::Error::custom)
    }
}

impl From<StorePath> for sea_orm::Value {
    fn from(p: StorePath) -> Self {
        sea_orm::Value::String(Some(p.base()))
    }
}

impl sea_orm::TryGetable for StorePath {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, sea_orm::TryGetError> {
        let raw = String::try_get_by(res, idx)?;
        StorePath::store_path_from_database_column_text(&raw)
            .map_err(|e| sea_orm::TryGetError::DbErr(sea_orm::DbErr::Type(e.to_string())))
    }
}

impl sea_orm::sea_query::ValueType for StorePath {
    fn try_from(v: sea_orm::Value) -> Result<Self, sea_orm::sea_query::ValueTypeErr> {
        match v {
            sea_orm::Value::String(Some(s)) => StorePath::store_path_from_database_column_text(&s)
                .map_err(|_| sea_orm::sea_query::ValueTypeErr),
            _ => Err(sea_orm::sea_query::ValueTypeErr),
        }
    }

    fn type_name() -> String {
        "StorePath".to_owned()
    }

    fn array_type() -> sea_orm::sea_query::ArrayType {
        sea_orm::sea_query::ArrayType::String
    }

    fn column_type() -> sea_orm::sea_query::ColumnType {
        sea_orm::sea_query::ColumnType::Text
    }
}

impl sea_orm::sea_query::Nullable for StorePath {
    fn null() -> sea_orm::Value {
        sea_orm::Value::String(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorePathError {
    Malformed(String),
}

impl fmt::Display for StorePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorePathError::Malformed(s) => write!(f, "malformed store path: {s}"),
        }
    }
}

impl std::error::Error for StorePathError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_original_path_needs_a_full_nix32_hash() {
        let hash = "0123456789abcdfghijklmnpqrsvwxyz";
        let path = OriginalStorePath::parse(&format!("/nix/store/{hash}-hello-2.12.1")).unwrap();
        assert_eq!(path.hash().as_str(), hash);
        assert_eq!(path.name(), "hello-2.12.1");
        assert_eq!(path.full(), format!("/nix/store/{hash}-hello-2.12.1"));
        assert_eq!(OriginalStorePath::parse(&path.base()).unwrap(), path);

        assert!(OriginalStorePath::parse("abc123-hello").is_err());
        assert!(OriginalStorePath::parse("0123456789abcdfghijklmnpqrsvwxye-hello").is_err());
        assert!(OriginalStorePath::parse(&format!("{hash}-")).is_err());
        assert!(StorePath::parse_stored("abc123-hello").is_err());
    }

    #[test]
    fn parse_reads_hash_name_and_derivation_flag() {
        let p = StorePath::parse_stored("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1")
            .unwrap();
        assert_eq!(p.hash(), "0123456789abcdfghijklmnpqrsvwxyz");
        assert_eq!(p.name(), "hello-2.12.1");
        assert!(!p.is_derivation());

        let p =
            StorePath::parse_stored("0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1.drv").unwrap();
        assert_eq!(p.hash(), "0123456789abcdfghijklmnpqrsvwxyz");
        assert_eq!(p.name(), "hello-2.12.1.drv");
        assert!(p.is_derivation());
    }

    #[test]
    fn full_and_base_round_trip_and_display_blanks_the_prefix() {
        let p = StorePath::from_hash_and_name_without_validation(
            "0123456789abcdfghijklmnpqrsvwxyz",
            "hello-2.12.1",
        );
        assert_eq!(p.base(), "0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1");
        assert_eq!(
            p.full(),
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1"
        );
        assert_eq!(StorePath::parse_stored(&p.full()).unwrap(), p);
        assert_eq!(StorePath::parse_stored(&p.base()).unwrap(), p);

        let drv = StorePath::from_hash_and_name_without_validation(
            "0123456789abcdfghijklmnpqrsvwxyz",
            "hello.drv",
        );
        assert_eq!(
            drv.to_string(),
            "0123456789abcdfghijklmnpqrsvwxyz-hello.drv"
        );
    }

    #[test]
    fn serde_uses_base_form() {
        let p = StorePath::from_hash_and_name_without_validation(
            "0123456789abcdfghijklmnpqrsvwxyz",
            "hello-2.12.1",
        );
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1\"");

        let back: StorePath =
            serde_json::from_str("\"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1\"")
                .unwrap();
        assert_eq!(back, p);
        let from_base: StorePath =
            serde_json::from_str("\"0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1\"").unwrap();
        assert_eq!(from_base, p);
    }

    #[test]
    fn rejects_malformed() {
        assert!(StorePath::parse_stored("nodash").is_err());
        assert!(StorePath::parse_stored("-noname").is_err());
        assert!(StorePath::parse_stored("nohash-").is_err());
        assert!(StorePath::parse_stored("/nix/store/").is_err());
    }

    const VALID_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    #[test]
    fn parse_stored_rejects_invalid_store_path_names_and_hashes() {
        let long_name = "a".repeat(212);
        let refused_names = [
            "hello\nStorePath: x",
            "hello world",
            "hello/world",
            "hello\0",
            ".",
            "..",
            ".-x",
            "..-x",
            "",
            long_name.as_str(),
        ];
        for name in refused_names {
            assert!(
                StorePath::parse_stored(&format!("{VALID_HASH}-{name}")).is_err(),
                "name {name:?} must be refused"
            );
        }

        let hash_of_33_characters = format!("{VALID_HASH}a");
        let refused_hashes = [
            &VALID_HASH[..31],
            hash_of_33_characters.as_str(),
            "0123456789abcdfghijklmnpqrsvwxye",
            "0123456789abcdfghijklmnpqrsvwxyo",
            "0123456789abcdfghijklmnpqrsvwxyt",
            "0123456789abcdfghijklmnpqrsvwxyu",
            "0123456789ABCDFGHIJKLMNPQRSVWXYZ",
        ];
        for hash in refused_hashes {
            assert!(
                StorePath::parse_stored(&format!("{hash}-hello")).is_err(),
                "hash {hash:?} must be refused"
            );
            assert!(OriginalStorePath::parse(&format!("{hash}-hello")).is_err());
        }

        assert!(OriginalStorePath::parse(&format!("{VALID_HASH}-hello\n")).is_err());
        assert!(serde_json::from_str::<StorePath>(&format!("\"{VALID_HASH}-a b\"")).is_err());
        assert!(format!("{VALID_HASH}-a/b").parse::<StorePath>().is_err());
    }

    #[test]
    fn parse_stored_accepts_valid_store_paths_of_every_kind() {
        let longest_name = "a".repeat(211);
        let accepted_names = [
            "hello-2.12.1",
            "hello-2.12.1.drv",
            "gtk+-3.24.0",
            "source?ref=main",
            "foo=bar",
            "under_score",
            ".gitignore",
            "..hidden",
            longest_name.as_str(),
        ];
        for name in accepted_names {
            let path = StorePath::parse_stored(&format!("/nix/store/{VALID_HASH}-{name}"))
                .unwrap_or_else(|_| panic!("name {name:?} must be accepted"));
            assert_eq!(path.hash(), VALID_HASH);
            assert_eq!(path.name(), name);
        }
    }

    #[test]
    fn database_column_text_with_short_legacy_hash_loads_into_store_path_while_parse_stored_rejects_it()
     {
        use sea_orm::sea_query::ValueType;
        let column = sea_orm::Value::String(Some("abc123-hello".to_owned()));
        let path = <StorePath as ValueType>::try_from(column).unwrap();
        assert_eq!(path.hash(), "abc123");
        assert!(StorePath::parse_stored("abc123-hello").is_err());
    }

    #[test]
    fn sea_orm_value_round_trip_is_prefix_free() {
        use sea_orm::sea_query::ValueType;
        let p = StorePath::from_hash_and_name_without_validation("abc123", "hello-2.12.1");
        let v = sea_orm::Value::from(p.clone());
        assert_eq!(v, sea_orm::Value::String(Some(p.base())));
        assert_eq!(<StorePath as ValueType>::try_from(v).unwrap(), p);
        let legacy = sea_orm::Value::String(Some(p.full()));
        assert_eq!(<StorePath as ValueType>::try_from(legacy).unwrap(), p);
    }
}
