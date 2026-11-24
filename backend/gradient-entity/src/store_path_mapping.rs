/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::sync::Arc;

use futures::future::BoxFuture;

use crate::ids::{CacheId, ProjectId};
use crate::store_path::{
    OriginalStorePath, STORE_DIR, STORE_HASH_LENGTH, StoreHash, StorePath, StorePathError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MappingContext {
    Project(ProjectId),
    Cache(CacheId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappingError(String);

impl MappingError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MappingError {}

pub trait StorePathMapping: Send + Sync {
    fn resolve(
        &self,
        context: MappingContext,
    ) -> BoxFuture<'_, Result<MappingContext, MappingError>> {
        Box::pin(std::future::ready(Ok(context)))
    }

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
    pub fn for_cache(mapping: Arc<dyn StorePathMapping>, cache: CacheId) -> Self {
        Self {
            mapping,
            context: MappingContext::Cache(cache),
        }
    }

    pub async fn for_project(
        mapping: Arc<dyn StorePathMapping>,
        project: ProjectId,
    ) -> Result<Self, MappingError> {
        let context = mapping.resolve(MappingContext::Project(project)).await?;
        Ok(Self { mapping, context })
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

    pub fn inbound_hash(&self, hash: StoreHash) -> StoreHash {
        self.mapping.inbound(self.context, hash)
    }

    pub fn inbound_text_if_exact_store_path(&self, text: &str) -> Result<String, StorePathError> {
        let path = OriginalStorePath::parse(text)?;
        Ok(TextParts::split(text).join(&self.mapping.inbound(self.context, path.hash().clone())))
    }

    pub fn stored_hash_text_from_inbound_hash_text(
        &self,
        text: &str,
    ) -> Result<String, StorePathError> {
        Ok(self.inbound_hash(StoreHash::parse(text)?).to_string())
    }

    pub fn map_and_validate_build_product_path(
        &self,
        text: &str,
    ) -> Result<String, StorePathError> {
        let malformed = || StorePathError::Malformed(text.to_owned());
        let base_and_sub_path = text
            .strip_prefix(STORE_DIR)
            .and_then(|rest| rest.strip_prefix('/'))
            .ok_or_else(malformed)?;
        let (base_name, sub_path) = match base_and_sub_path.split_once('/') {
            Some((base_name, sub_path)) => (base_name, Some(sub_path)),
            None => (base_and_sub_path, None),
        };
        if !sub_path.is_none_or(|sub_path| sub_path.split('/').all(is_acceptable_sub_path_segment))
        {
            return Err(malformed());
        }
        let stored = self
            .inbound(
                &OriginalStorePath::parse_store_path_base_name(base_name)
                    .map_err(|_| malformed())?,
            )
            .full();
        Ok(match sub_path {
            Some(sub_path) => format!("{stored}/{sub_path}"),
            None => stored,
        })
    }

    pub fn outbound_text(&self, text: &str) -> Result<String, StorePathError> {
        let parts = TextParts::split(text);
        if parts.hash.is_empty() {
            return Err(StorePathError::Malformed(text.to_owned()));
        }
        let hash = StoreHash::stored(parts.hash.to_owned());
        Ok(parts.join(&self.mapping.outbound(self.context, hash)))
    }

    pub fn rewrite_stored_store_paths_in_text_to_outbound(&self, text: &str) -> String {
        let mut rewritten = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(index) = rest.find(STORE_DIR) {
            let (before, after_store_prefix) = rest.split_at(index + STORE_DIR.len());
            rewritten.push_str(before);
            rest = after_store_prefix;
            if let Some(hash) = store_hash_from_text_after_store_prefix(rest) {
                rewritten.push('/');
                rewritten.push_str(self.mapping.outbound(self.context, hash).as_str());
                rest = &rest[1 + STORE_HASH_LENGTH..];
            }
        }
        rewritten.push_str(rest);
        rewritten
    }

    pub fn rewrite_stored_drv_paths_in_json_to_outbound(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, field) in fields.iter_mut() {
                    match field {
                        serde_json::Value::String(path) if key == "drv_path" => {
                            if let Ok(original) = self.outbound_text(path) {
                                *path = original;
                            }
                        }
                        _ => self.rewrite_stored_drv_paths_in_json_to_outbound(field),
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    self.rewrite_stored_drv_paths_in_json_to_outbound(item);
                }
            }
            _ => {}
        }
    }

    pub fn inbound_if_store_path(&self, text: &str) -> Result<String, StorePathError> {
        match text.starts_with(STORE_DIR) {
            true => self.map_and_validate_build_product_path(text),
            false => Ok(text.to_owned()),
        }
    }

    pub fn outbound_if_store_path(&self, text: &str) -> String {
        match text.starts_with(STORE_DIR) {
            true => self.outbound_text(text).unwrap_or_else(|_| text.to_owned()),
            false => text.to_owned(),
        }
    }

    pub fn inbound_prefix(&self, prefix: &str) -> Option<String> {
        self.mapping.inbound_prefix(self.context, prefix)
    }
}

fn store_hash_from_text_after_store_prefix(text: &str) -> Option<StoreHash> {
    let base_name = text.strip_prefix('/')?;
    let hash = base_name.get(..STORE_HASH_LENGTH)?;
    if !base_name[STORE_HASH_LENGTH..].starts_with('-') {
        return None;
    }
    StoreHash::parse(hash).ok()
}

fn is_acceptable_sub_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !matches!(segment, "." | "..")
        && !segment.chars().any(|c| c == '\\' || c.is_control())
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
        let alphabet: Vec<char> = crate::NIX32_ALPHABET.chars().collect();
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
        StoreHash::stored(Self::rotate(hash.as_str(), crate::NIX32_ALPHABET.len() - 1))
    }

    fn inbound_prefix(&self, _context: MappingContext, prefix: &str) -> Option<String> {
        Some(Self::rotate(prefix, 1))
    }
}

#[cfg(feature = "testing")]
#[derive(Clone, Copy, Debug, Default)]
pub struct UnresolvableProjectMapping;

#[cfg(feature = "testing")]
impl StorePathMapping for UnresolvableProjectMapping {
    fn resolve(
        &self,
        context: MappingContext,
    ) -> BoxFuture<'_, Result<MappingContext, MappingError>> {
        let resolved = match context {
            MappingContext::Project(project) => Err(MappingError::new(format!(
                "project {project} has no mapping context"
            ))),
            MappingContext::Cache(_) => Ok(context),
        };
        Box::pin(std::future::ready(resolved))
    }

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

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;

    fn rotated() -> StorePathMapper {
        StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil())
    }

    struct ProjectCacheMapping(CacheId);

    impl StorePathMapping for ProjectCacheMapping {
        fn resolve(
            &self,
            context: MappingContext,
        ) -> BoxFuture<'_, Result<MappingContext, MappingError>> {
            let resolved = match context {
                MappingContext::Project(_) => MappingContext::Cache(self.0),
                MappingContext::Cache(_) => context,
            };
            Box::pin(std::future::ready(Ok(resolved)))
        }

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

    #[tokio::test]
    async fn a_project_mapper_maps_with_the_resolved_context() {
        let cache = CacheId::new(uuid::Uuid::from_u128(0x5c4e_d2a1));
        let project = ProjectId::new(uuid::Uuid::from_u128(0x9b07_e113));
        let mapper = StorePathMapper::for_project(Arc::new(ProjectCacheMapping(cache)), project)
            .await
            .unwrap();
        assert_eq!(mapper.context(), MappingContext::Cache(cache));

        let identity = StorePathMapper::for_project(Arc::new(IdentityMapping), project)
            .await
            .unwrap();
        assert_eq!(identity.context(), MappingContext::Project(project));
    }

    #[test]
    fn drv_paths_inside_job_context_are_mapped_outbound() {
        let (original, stored) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let mut value = serde_json::json!({
            "derivations": [{ "drv_path": format!("{stored}-a.drv"), "name": "a" }]
        });

        rotated().rewrite_stored_drv_paths_in_json_to_outbound(&mut value);

        assert_eq!(
            value,
            serde_json::json!({
                "derivations": [{ "drv_path": format!("{original}-a.drv"), "name": "a" }]
            })
        );
    }

    #[test]
    fn rewrite_stored_store_paths_in_free_text_to_original_leaves_other_text_unchanged() {
        let mapper = StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil());
        let text = format!(
            "closure of /nix/store/{EXAMPLE_STORED_HASH}-a.drv^out needs /nix/store/{EXAMPLE_STORED_HASH}-b, see /nix/store/short-c"
        );
        assert_eq!(
            mapper.rewrite_stored_store_paths_in_text_to_outbound(&text),
            format!(
                "closure of /nix/store/{EXAMPLE_ORIGINAL_HASH}-a.drv^out needs /nix/store/{EXAMPLE_ORIGINAL_HASH}-b, see /nix/store/short-c"
            )
        );
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
        let mapper = rotated();
        assert_eq!(
            mapper
                .map_and_validate_build_product_path(&format!("/nix/store/{o}-hello/share/doc"))
                .unwrap(),
            format!("/nix/store/{s}-hello/share/doc")
        );
        assert_eq!(
            mapper
                .inbound_text_if_exact_store_path(&format!("{o}-hello.drv"))
                .unwrap(),
            format!("{s}-hello.drv")
        );
        assert_eq!(
            mapper.stored_hash_text_from_inbound_hash_text(o).unwrap(),
            s
        );
        assert_eq!(
            mapper
                .outbound_text(&format!("/nix/store/{s}-hello"))
                .unwrap(),
            format!("/nix/store/{o}-hello")
        );
        assert!(
            mapper
                .inbound_text_if_exact_store_path("abc-hello")
                .is_err()
        );
    }

    #[test]
    fn inbound_text_rejects_invalid_store_path_names_and_maps_hash_and_sub_path() {
        let o = EXAMPLE_ORIGINAL_HASH;
        let mapper = rotated();
        for refused in [
            format!("{o}-{}", "a".repeat(212)),
            format!("{o}-hello~world"),
            format!("/nix/store/{o}-hello world/share"),
            format!("{o}-"),
        ] {
            assert!(
                mapper.inbound_text_if_exact_store_path(&refused).is_err(),
                "{refused}"
            );
            assert!(
                mapper
                    .map_and_validate_build_product_path(&refused)
                    .is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn maps_exactly_one_store_path_only_for_full_path_or_base_name() {
        let (o, s) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let mapper = rotated();
        for refused in [
            format!("/nix/store/{o}-hello/bin/hello"),
            format!("/nix/store/{o}-hello/../x"),
            format!("{o}-hello/../x"),
            o.to_owned(),
        ] {
            assert!(
                mapper.inbound_text_if_exact_store_path(&refused).is_err(),
                "{refused}"
            );
        }
        assert_eq!(
            mapper
                .inbound_text_if_exact_store_path(&format!("/nix/store/{o}-hello"))
                .unwrap(),
            format!("/nix/store/{s}-hello")
        );
        assert_eq!(
            mapper
                .inbound_text_if_exact_store_path(&format!("{o}-hello.drv"))
                .unwrap(),
            format!("{s}-hello.drv")
        );
        assert_eq!(
            mapper.stored_hash_text_from_inbound_hash_text(o).unwrap(),
            s
        );
        assert!(
            mapper
                .stored_hash_text_from_inbound_hash_text(&format!("{o}-hello"))
                .is_err()
        );
    }

    #[test]
    fn rejects_bad_build_product_sub_path_segments() {
        let (o, s) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let mapper = rotated();
        for refused in [
            format!("/nix/store/{o}-hello//bin"),
            format!("/nix/store/{o}-hello/bin/"),
            format!("/nix/store/{o}-hello/./bin"),
            format!("/nix/store/{o}-hello/../x"),
            format!("/nix/store/{o}-hello/bin\\tool"),
            format!("/nix/store/{o}-hello/bin\0tool"),
            format!("/nix/store/{o}-hello/bin\ntool"),
            format!("/nix/store/{o}-hello~/bin"),
            o.to_owned(),
        ] {
            assert!(
                mapper
                    .map_and_validate_build_product_path(&refused)
                    .is_err(),
                "{refused:?}"
            );
        }
        for (original, stored) in [
            (
                format!("/nix/store/{o}-hello"),
                format!("/nix/store/{s}-hello"),
            ),
            (
                format!("/nix/store/{o}-hello/bin/tool"),
                format!("/nix/store/{s}-hello/bin/tool"),
            ),
            (
                format!("/nix/store/{o}-hello/share/doc/a b.txt"),
                format!("/nix/store/{s}-hello/share/doc/a b.txt"),
            ),
        ] {
            assert_eq!(
                mapper
                    .map_and_validate_build_product_path(&original)
                    .unwrap(),
                stored
            );
        }
    }

    #[test]
    fn only_a_store_path_repository_is_mapped_outbound() {
        let (o, s) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let mapper = rotated();
        assert_eq!(
            mapper.outbound_if_store_path(&format!("/nix/store/{s}-source")),
            format!("/nix/store/{o}-source")
        );
        let url = "https://example.invalid/my-repo";
        assert_eq!(mapper.outbound_if_store_path(url), url);
    }
}
