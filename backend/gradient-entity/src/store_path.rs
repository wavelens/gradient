/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The canonical Nix store directory.
pub const STORE_DIR: &str = "/nix/store";

/// A Nix store path decomposed into its `<hash>-<name>` parts.
///
/// Stored, compared, and serialized without the `/nix/store/` prefix - the
/// prefix is a presentation concern reconstructed via [`StorePath::full`] only
/// where a real filesystem path is needed (build dispatch, worker store
/// operations, the binary-cache protocol). `name` retains a trailing `.drv`
/// for derivations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StorePath {
    hash: String,
    name: String,
}

impl StorePath {
    /// Build from already-separated columns (e.g. an entity row). Infallible;
    /// trusts the caller, who holds parts that originate from Nix or the DB.
    pub fn from_parts(hash: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            hash: hash.into(),
            name: name.into(),
        }
    }

    /// Parse either a full `/nix/store/<hash>-<name>` path or a bare
    /// `<hash>-<name>` base. Validates only the structure (a `-` separator with
    /// non-empty parts); the hash alphabet is guaranteed upstream by Nix.
    pub fn parse(input: &str) -> Result<Self, StorePathError> {
        let base = input
            .strip_prefix(STORE_DIR)
            .map(|rest| rest.trim_start_matches('/'))
            .unwrap_or(input);

        let (hash, name) = base
            .split_once('-')
            .ok_or_else(|| StorePathError::Malformed(input.to_owned()))?;

        if hash.is_empty() || name.is_empty() {
            return Err(StorePathError::Malformed(input.to_owned()));
        }

        Ok(Self::from_parts(hash, name))
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this path is a derivation (`.drv`).
    pub fn is_derivation(&self) -> bool {
        self.name.ends_with(".drv")
    }

    /// Prefix-free `<hash>-<name>` form, used on the API and wire.
    pub fn base(&self) -> String {
        format!("{}-{}", self.hash, self.name)
    }

    /// Full `/nix/store/<hash>-<name>` path, used for dispatch, worker store
    /// operations, and the binary-cache protocol.
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
        Self::parse(s)
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
        StorePath::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl From<StorePath> for sea_orm::Value {
    fn from(p: StorePath) -> Self {
        sea_orm::Value::String(Some(Box::new(p.base())))
    }
}

impl sea_orm::TryGetable for StorePath {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, sea_orm::TryGetError> {
        let raw = String::try_get_by(res, idx)?;
        StorePath::parse(&raw)
            .map_err(|e| sea_orm::TryGetError::DbErr(sea_orm::DbErr::Type(e.to_string())))
    }
}

impl sea_orm::sea_query::ValueType for StorePath {
    fn try_from(v: sea_orm::Value) -> Result<Self, sea_orm::sea_query::ValueTypeErr> {
        match v {
            sea_orm::Value::String(Some(s)) => {
                StorePath::parse(&s).map_err(|_| sea_orm::sea_query::ValueTypeErr)
            }
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
    fn parses_full_path() {
        let p = StorePath::parse("/nix/store/abc123-hello-2.12.1").unwrap();
        assert_eq!(p.hash(), "abc123");
        assert_eq!(p.name(), "hello-2.12.1");
        assert!(!p.is_derivation());
    }

    #[test]
    fn parses_bare_base() {
        let p = StorePath::parse("abc123-hello-2.12.1.drv").unwrap();
        assert_eq!(p.hash(), "abc123");
        assert_eq!(p.name(), "hello-2.12.1.drv");
        assert!(p.is_derivation());
    }

    #[test]
    fn full_and_base_roundtrip() {
        let p = StorePath::from_parts("abc123", "hello-2.12.1");
        assert_eq!(p.base(), "abc123-hello-2.12.1");
        assert_eq!(p.full(), "/nix/store/abc123-hello-2.12.1");
        assert_eq!(StorePath::parse(&p.full()).unwrap(), p);
        assert_eq!(StorePath::parse(&p.base()).unwrap(), p);
    }

    #[test]
    fn display_is_prefix_free() {
        let p = StorePath::from_parts("abc123", "hello.drv");
        assert_eq!(p.to_string(), "abc123-hello.drv");
    }

    #[test]
    fn serde_uses_base_form() {
        let p = StorePath::from_parts("abc123", "hello-2.12.1");
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"abc123-hello-2.12.1\"");

        let back: StorePath = serde_json::from_str("\"/nix/store/abc123-hello-2.12.1\"").unwrap();
        assert_eq!(back, p);
        let from_base: StorePath = serde_json::from_str("\"abc123-hello-2.12.1\"").unwrap();
        assert_eq!(from_base, p);
    }

    #[test]
    fn rejects_malformed() {
        assert!(StorePath::parse("nodash").is_err());
        assert!(StorePath::parse("-noname").is_err());
        assert!(StorePath::parse("nohash-").is_err());
        assert!(StorePath::parse("/nix/store/").is_err());
    }

    #[test]
    fn sea_orm_value_round_trip_is_prefix_free() {
        use sea_orm::sea_query::ValueType;
        let p = StorePath::from_parts("abc123", "hello-2.12.1");
        let v = sea_orm::Value::from(p.clone());
        assert_eq!(v, sea_orm::Value::String(Some(Box::new(p.base()))));
        assert_eq!(<StorePath as ValueType>::try_from(v).unwrap(), p);
        // Full-path values written before the typed column parse identically.
        let legacy = sea_orm::Value::String(Some(Box::new(p.full())));
        assert_eq!(<StorePath as ValueType>::try_from(legacy).unwrap(), p);
    }
}
