/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::sync::Arc;

use crate::ids::{CacheId, ProjectId};
use crate::store_path::{
    NIX32_ALPHABET, OriginalStorePath, STORE_DIR, StoreHash, StorePath, StorePathError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MappingContext {
    Project(ProjectId),
    Cache(CacheId),
}

pub trait StorePathMapping: Send + Sync {
    fn inbound(&self, context: MappingContext, hash: StoreHash) -> StoreHash;
    fn outbound(&self, context: MappingContext, hash: StoreHash) -> StoreHash;
    fn inbound_prefix(&self, context: MappingContext, prefix: &str) -> Option<String>;
}

impl fmt::Debug for dyn StorePathMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StorePathMapping")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IdentityMapping;

impl StorePathMapping for IdentityMapping {
    fn inbound(&self, _context: MappingContext, hash: StoreHash) -> StoreHash {
        hash
    }

    fn outbound(&self, _context: MappingContext, hash: StoreHash) -> StoreHash {
        hash
    }

    fn inbound_prefix(&self, _context: MappingContext, prefix: &str) -> Option<String> {
        Some(prefix.to_owned())
    }
}

#[derive(Clone, Debug)]
pub struct StorePathMapper {
    mapping: Arc<dyn StorePathMapping>,
    context: MappingContext,
}

impl StorePathMapper {
    pub fn new(mapping: Arc<dyn StorePathMapping>, context: MappingContext) -> Self {
        Self { mapping, context }
    }

    pub fn context(&self) -> MappingContext {
        self.context
    }

    pub fn inbound(&self, path: &OriginalStorePath) -> StorePath {
        let hash = self.mapping.inbound(self.context, path.hash().clone());
        StorePath::from_hash(hash, path.name())
    }

    pub fn outbound(&self, path: &StorePath) -> OriginalStorePath {
        let hash = self
            .mapping
            .outbound(self.context, path.store_hash().clone());
        OriginalStorePath::from_hash(hash, path.name())
    }

    pub fn inbound_text(&self, text: &str) -> Result<String, StorePathError> {
        let parts = TextParts::split(text);
        let hash =
            StoreHash::parse(parts.hash).map_err(|_| StorePathError::Malformed(text.to_owned()))?;
        Ok(parts.join(&self.mapping.inbound(self.context, hash)))
    }

    pub fn outbound_text(&self, text: &str) -> Result<String, StorePathError> {
        let parts = TextParts::split(text);
        if parts.hash.is_empty() {
            return Err(StorePathError::Malformed(text.to_owned()));
        }
        let hash = StoreHash::stored(parts.hash.to_owned());
        Ok(parts.join(&self.mapping.outbound(self.context, hash)))
    }

    pub fn inbound_prefix(&self, prefix: &str) -> Option<String> {
        self.mapping.inbound_prefix(self.context, prefix)
    }
}

struct TextParts<'a> {
    store_dir: &'a str,
    hash: &'a str,
    rest: &'a str,
}

impl<'a> TextParts<'a> {
    fn split(text: &'a str) -> Self {
        let store_dir_length = text
            .strip_prefix(STORE_DIR)
            .and_then(|rest| rest.strip_prefix('/'))
            .map_or(0, |_| STORE_DIR.len() + 1);
        let (store_dir, base) = text.split_at(store_dir_length);
        let hash_length = base.find('-').unwrap_or(base.len());
        let (hash, rest) = base.split_at(hash_length);
        Self {
            store_dir,
            hash,
            rest,
        }
    }

    fn join(&self, hash: &StoreHash) -> String {
        format!("{}{}{}", self.store_dir, hash, self.rest)
    }
}

#[cfg(feature = "testing")]
pub const EXAMPLE_ORIGINAL_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
#[cfg(feature = "testing")]
pub const EXAMPLE_STORED_HASH: &str = "123456789abcdfghijklmnpqrsvwxyz0";

#[cfg(feature = "testing")]
#[derive(Clone, Copy, Debug, Default)]
pub struct RotatedHashMapping;

#[cfg(feature = "testing")]
impl RotatedHashMapping {
    fn rotate(text: &str, steps: usize) -> String {
        let alphabet: Vec<char> = NIX32_ALPHABET.chars().collect();
        text.chars()
            .map(|c| match alphabet.iter().position(|&a| a == c) {
                Some(index) => alphabet[(index + steps) % alphabet.len()],
                None => c,
            })
            .collect()
    }
}

#[cfg(feature = "testing")]
impl StorePathMapping for RotatedHashMapping {
    fn inbound(&self, _context: MappingContext, hash: StoreHash) -> StoreHash {
        StoreHash::stored(Self::rotate(hash.as_str(), 1))
    }

    fn outbound(&self, _context: MappingContext, hash: StoreHash) -> StoreHash {
        StoreHash::stored(Self::rotate(hash.as_str(), NIX32_ALPHABET.len() - 1))
    }

    fn inbound_prefix(&self, _context: MappingContext, prefix: &str) -> Option<String> {
        Some(Self::rotate(prefix, 1))
    }
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;

    fn rotated(context: MappingContext) -> StorePathMapper {
        StorePathMapper::new(Arc::new(RotatedHashMapping), context)
    }

    #[test]
    fn both_mappings_return_the_hash_they_were_given() {
        let hash = StoreHash::parse(EXAMPLE_ORIGINAL_HASH).unwrap();
        let mappings: [Arc<dyn StorePathMapping>; 2] =
            [Arc::new(IdentityMapping), Arc::new(RotatedHashMapping)];
        for mapping in mappings {
            for context in [
                MappingContext::Project(ProjectId::nil()),
                MappingContext::Cache(CacheId::nil()),
            ] {
                let stored = mapping.inbound(context, hash.clone());
                assert_eq!(mapping.outbound(context, stored), hash);
            }
        }
    }

    #[test]
    fn the_rotated_mapping_moves_every_character_one_step() {
        let hash = StoreHash::parse(EXAMPLE_ORIGINAL_HASH).unwrap();
        let context = MappingContext::Cache(CacheId::nil());
        assert_eq!(
            RotatedHashMapping.inbound(context, hash).as_str(),
            EXAMPLE_STORED_HASH
        );
        assert_eq!(
            RotatedHashMapping.inbound_prefix(context, "012").as_deref(),
            Some("123")
        );
        assert_eq!(
            IdentityMapping.inbound_prefix(context, "012").as_deref(),
            Some("012")
        );
    }

    #[test]
    fn text_mapping_swaps_only_the_hash() {
        let (o, s) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let mapper = rotated(MappingContext::Project(ProjectId::nil()));
        assert_eq!(
            mapper
                .inbound_text(&format!("/nix/store/{o}-hello/share/doc"))
                .unwrap(),
            format!("/nix/store/{s}-hello/share/doc")
        );
        assert_eq!(
            mapper.inbound_text(&format!("{o}-hello.drv")).unwrap(),
            format!("{s}-hello.drv")
        );
        assert_eq!(mapper.inbound_text(o).unwrap(), s);
        assert_eq!(
            mapper
                .outbound_text(&format!("/nix/store/{s}-hello"))
                .unwrap(),
            format!("/nix/store/{o}-hello")
        );
        assert!(mapper.inbound_text("abc-hello").is_err());
    }
}
