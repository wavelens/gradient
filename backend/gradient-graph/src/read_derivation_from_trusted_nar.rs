/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use futures::StreamExt as _;
use gradient_db::store_path_content_variant_reader_choice::trusted_content_variants_by_store_path_hash_for_project_best_ranked_first_up_to_count;
use gradient_derivation::{Derivation, discovered_derivation, parse_drv};
use gradient_storage::nar_extract::nar_reader_from_stream;
use gradient_storage::{NarSource, NarStore};
use gradient_types::*;
use gradient_util::nar::single_file_nar_frame;
use gradient_wire::content_addressed_store_path_from_name_address_references::path_for;
use gradient_wire::types::DiscoveredDerivation;
use harmonia_store_content_address::ContentAddress;
use harmonia_store_path::{StorePath as NixStorePath, StorePathSet};
use harmonia_utils_hash::Sha256;
use sea_orm::ConnectionTrait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncReadExt as _;

pub const MAX_DRV_FILE_BYTES_READ_INTO_MEMORY: u64 = 64 * 1024 * 1024;
pub const MAX_TRUSTED_DRV_CONTENT_VARIANTS_READ_AND_VERIFIED_PER_FILE: i64 = 4;

/// Only the reader in this module can construct this record. Every derivation record that the graph writer can save must come from a stored derivation file with text identical to the store path.
#[derive(Debug, Clone, PartialEq)]
pub struct StorePathVerifiedDerivationRecord {
    derivation: DiscoveredDerivation,
    original_client_derivation_store_path: String,
    verified_derivation_file_bytes: Arc<[u8]>,
}

impl StorePathVerifiedDerivationRecord {
    pub fn to_discovered_derivation(&self) -> DiscoveredDerivation {
        self.derivation.clone()
    }

    pub fn derivation_file_store_path(&self) -> &str {
        &self.derivation.drv_path
    }

    pub fn original_client_derivation_store_path(&self) -> &str {
        &self.original_client_derivation_store_path
    }

    pub fn verified_derivation_file_bytes(&self) -> &[u8] {
        &self.verified_derivation_file_bytes
    }
}

pub fn map_verified_derivation_records_by_store_path<'a>(
    records: impl IntoIterator<Item = &'a StorePathVerifiedDerivationRecord>,
) -> anyhow::Result<HashMap<String, StorePathVerifiedDerivationRecord>> {
    records
        .into_iter()
        .map(|record| {
            let path = record.derivation_file_store_path();
            let stored = StorePath::parse_stored(path).map_err(|error| {
                anyhow::anyhow!("Reported derivation file {path} has no valid store path: {error}")
            })?;
            Ok((stored.full(), record.clone()))
        })
        .collect()
}

#[cfg(test)]
impl crate::messages::EvaluationBatchDerivation {
    pub(crate) fn test_batch_entry_from_discovered_derivation(
        derivation: DiscoveredDerivation,
    ) -> Self {
        Self {
            attr: derivation.attr.clone(),
            ifd: derivation.ifd,
            licenses: derivation.licenses.clone(),
            exec: derivation.exec.clone(),
            store_path_verified_derivation_record: StorePathVerifiedDerivationRecord {
                derivation,
                original_client_derivation_store_path: String::new(),
                verified_derivation_file_bytes: Arc::from([]),
            },
            closure_checked_hash_modulo: Some(crate::derivation_closure_input_addressed_output_verification::VerifiedDerivationHashModulo::verified_derivation_hash_modulo_deferred_form_for_tests()),
        }
    }
}

pub async fn load_verified_trusted_derivation_record<C: ConnectionTrait>(
    db: &C,
    nar_storage: &NarStore,
    mapping: Arc<dyn StorePathMapping>,
    project: ProjectId,
    drv_path: &str,
) -> anyhow::Result<Option<StorePathVerifiedDerivationRecord>> {
    let stored = StorePath::parse_stored(drv_path)?;
    anyhow::ensure!(
        stored.is_derivation(),
        "{drv_path} is not a derivation file"
    );

    let variants =
        trusted_content_variants_by_store_path_hash_for_project_best_ranked_first_up_to_count(
            db,
            project,
            stored.hash(),
            MAX_TRUSTED_DRV_CONTENT_VARIANTS_READ_AND_VERIFIED_PER_FILE,
        )
        .await?;
    if variants.is_empty() {
        return Ok(None);
    }

    let mapper = StorePathMapper::for_project(mapping, project).await?;
    let mut refusal = None;
    for variant in &variants {
        match read_derivation_variant_from_trusted_nar_and_verify_store_path_hashes(
            nar_storage,
            &mapper,
            &stored,
            drv_path,
            &variant.nar_object_storage_key,
        )
        .await
        {
            Ok(Some(record)) => return Ok(Some(record)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    "Verification failed for the trusted copy of {drv_path} in storage. The server can check the next copy. {error}"
                );
                refusal = Some(error);
            }
        }
    }
    refusal.map_or(Ok(None), Err)
}

async fn read_derivation_variant_from_trusted_nar_and_verify_store_path_hashes(
    nar_storage: &NarStore,
    mapper: &StorePathMapper,
    stored: &StorePath,
    drv_path: &str,
    nar_object_storage_key: &str,
) -> anyhow::Result<Option<StorePathVerifiedDerivationRecord>> {
    let Some(source) = nar_storage.open(nar_object_storage_key, 0).await? else {
        return Ok(None);
    };
    let stream = match source {
        NarSource::Hot(bytes) => futures::stream::once(async move { Ok(bytes) }).boxed(),
        NarSource::Stream { stream, .. } => stream,
    };

    let mut nar = Vec::new();
    nar_reader_from_stream(stream)
        .take(MAX_DRV_FILE_BYTES_READ_INTO_MEMORY + 1)
        .read_to_end(&mut nar)
        .await?;
    anyhow::ensure!(
        nar.len() as u64 <= MAX_DRV_FILE_BYTES_READ_INTO_MEMORY,
        "The stored NAR for {drv_path} is larger than {limit} bytes",
        limit = MAX_DRV_FILE_BYTES_READ_INTO_MEMORY
    );
    let contents = read_single_regular_file_contents_from_nar(&nar).ok_or_else(|| {
        anyhow::anyhow!("The NAR for {drv_path} doesn't contain exactly one regular file.")
    })?;

    let drv = parse_drv(contents)?;
    let original = mapper.outbound(stored);
    let references = drv
        .input_derivations
        .iter()
        .map(|input| input.path.as_str())
        .chain(drv.input_sources.iter().map(String::as_str))
        .map(|path| {
            let base = path.strip_prefix(STORE_DIR).unwrap_or(path);
            NixStorePath::from_base_path(base.trim_start_matches('/'))
                .map_err(|e| anyhow::anyhow!("{path}: {e}"))
        })
        .collect::<anyhow::Result<StorePathSet>>()?;
    let computed = path_for(
        original.name(),
        &ContentAddress::Text(Sha256::digest(contents)),
        &references,
    )?
    .to_string();
    anyhow::ensure!(
        computed == original.base(),
        "The text of {drv_path} does not match its store path. The hash is {computed}."
    );

    let drv = with_stored_paths(mapper, drv)?;
    let mut derivation = discovered_derivation(None, stored.base(), &drv);
    for dependency in &mut derivation.dependencies {
        *dependency = StorePath::parse_stored(dependency)?.base();
    }
    Ok(Some(StorePathVerifiedDerivationRecord {
        derivation,
        original_client_derivation_store_path: original.base(),
        verified_derivation_file_bytes: Arc::from(contents),
    }))
}

fn with_stored_paths(mapper: &StorePathMapper, mut drv: Derivation) -> anyhow::Result<Derivation> {
    for output in drv.outputs.iter_mut().filter(|o| !o.path.is_empty()) {
        output.path = mapper.inbound_text_if_exact_store_path(&output.path)?;
    }
    for input in &mut drv.input_derivations {
        input.path = mapper.inbound_text_if_exact_store_path(&input.path)?;
    }
    for source in &mut drv.input_sources {
        *source = mapper.inbound_text_if_exact_store_path(source)?;
    }
    Ok(drv)
}

fn read_single_regular_file_contents_from_nar(nar: &[u8]) -> Option<&[u8]> {
    let (head, _) = single_file_nar_frame(0, false);
    let length = u64::from_le_bytes(nar.get(head.len() - 8..head.len())?.try_into().ok()?);
    let (head, tail) = single_file_nar_frame(length, false);
    let end = head.len().checked_add(usize::try_from(length).ok()?)?;
    (nar.starts_with(&head) && nar.get(end..)? == tail.as_slice()).then(|| &nar[head.len()..end])
}

#[cfg(test)]
mod tests;
