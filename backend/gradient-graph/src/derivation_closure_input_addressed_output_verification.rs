/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::read_derivation_from_trusted_nar::StorePathVerifiedDerivationRecord;
use futures::StreamExt as _;
use gradient_types::*;
use gradient_wire::types::DiscoveredDerivation;
use harmonia_store_aterm::hash_modulo::{HashModulo, hash_input_modulo, hash_modulo};
use harmonia_store_aterm::input_address::make_output_path;
use harmonia_store_aterm::parse_derivation_aterm;
use harmonia_store_derivation::derivation::{
    Derivation as OriginalDerivation, DerivationInputs, DerivationOutput as OriginalOutput,
};
use harmonia_store_path::{StoreDir, StorePath as ClientPath};
use harmonia_utils_hash::Sha256;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::future::Future;

const CONCURRENT_READS: usize = 32;
pub const INPUT_DRV_STORE_PATH_COUNT_PER_STORED_HASH_MODULO_DATABASE_QUERY: usize = 1000;

/// Only closure checks can create this value. They compute it from the verified `.drv` text for each store path. They do this only for derivations where the client path and the store path are equal. Projects that match that row all get the same value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDerivationHashModulo(String);

impl VerifiedDerivationHashModulo {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn verified_derivation_hash_modulo_deferred_form_for_tests() -> Self {
        Self(hash_modulo_to_prefixed_storage_text(
            &HashModulo::DeferredDrv,
        ))
    }
}

#[derive(Debug)]
pub struct DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath<D> {
    pub read_stored_derivation_path_and_derivation_file_record_pairs: Vec<(String, D)>,
    pub verified_hash_modulo_by_stored_derivation_path_for_equal_client_and_stored_paths:
        HashMap<String, VerifiedDerivationHashModulo>,
}

pub fn hash_modulo_to_prefixed_storage_text(hash: &HashModulo) -> String {
    let hex = |digest: &Sha256| {
        digest
            .digest_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    match hash {
        HashModulo::DrvHash(digest) => format!("drv:sha256:{}", hex(digest)),
        HashModulo::CaOutputHashes(outputs) => format!(
            "fixed:{}",
            outputs
                .iter()
                .map(|(output, digest)| format!("{output}=sha256:{}", hex(digest)))
                .collect::<Vec<_>>()
                .join(";")
        ),
        HashModulo::DeferredDrv => "deferred".to_owned(),
    }
}

pub fn hash_modulo_from_prefixed_storage_text(text: &str) -> Option<HashModulo> {
    let digest = |prefixed: &str| {
        let hex = prefixed.strip_prefix("sha256:")?;
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        Sha256::from_slice(&bytes).ok()
    };
    if text == "deferred" {
        return Some(HashModulo::DeferredDrv);
    }
    if let Some(prefixed) = text.strip_prefix("drv:") {
        return digest(prefixed).map(HashModulo::DrvHash);
    }
    let outputs = text.strip_prefix("fixed:")?;
    outputs
        .split(';')
        .map(|entry| {
            let (output, prefixed) = entry.split_once('=')?;
            Some((output.parse().ok()?, digest(prefixed)?))
        })
        .collect::<Option<_>>()
        .map(HashModulo::CaOutputHashes)
}

pub async fn load_stored_derivation_hash_modulo_text_by_store_path_for_chunk<C: ConnectionTrait>(
    db: &C,
    drv_paths: &[String],
) -> anyhow::Result<HashMap<String, String>> {
    let hashes = drv_paths
        .iter()
        .map(|path| Ok(StorePath::parse_stored(path)?.hash().to_owned()))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let wanted: HashSet<&str> = drv_paths.iter().map(String::as_str).collect();
    Ok(EDerivation::find()
        .filter(CDerivation::Hash.is_in(hashes))
        .filter(CDerivation::SerializedDerivationHashForDependentOutputPaths.is_not_null())
        .all(db)
        .await?
        .into_iter()
        .filter_map(|row| {
            let path = row.store_path();
            let text = row.serialized_derivation_hash_for_dependent_output_paths?;
            wanted.contains(path.as_str()).then_some((path, text))
        })
        .collect())
}

pub trait DerivationFileInBuildRequestClosure {
    fn stored_derivation(&self) -> DiscoveredDerivation;
    fn original_drv_store_path(&self) -> anyhow::Result<ClientPath>;
    fn derivation_with_original_paths(&self) -> anyhow::Result<OriginalDerivation>;
}

impl DerivationFileInBuildRequestClosure for StorePathVerifiedDerivationRecord {
    fn stored_derivation(&self) -> DiscoveredDerivation {
        self.to_discovered_derivation()
    }

    fn original_drv_store_path(&self) -> anyhow::Result<ClientPath> {
        let base = self.original_client_derivation_store_path();
        ClientPath::from_base_path(base).map_err(|e| anyhow::anyhow!("{base}: {e}"))
    }

    fn derivation_with_original_paths(&self) -> anyhow::Result<OriginalDerivation> {
        let original = self.original_drv_store_path()?;
        let name = original.name().to_string();
        let name = name.strip_suffix(".drv").unwrap_or(&name).parse()?;
        Ok(parse_derivation_aterm(
            &StoreDir::default(),
            self.verified_derivation_file_bytes(),
            name,
        )?)
    }
}

pub trait DrvSource: Sync {
    type Drv: DerivationFileInBuildRequestClosure + Send;

    fn read(
        &self,
        drv_path: &str,
    ) -> impl Future<Output = anyhow::Result<Option<Self::Drv>>> + Send;

    fn read_stored_derivation_hash_modulo_text_for_drv_paths(
        &self,
        drv_paths: &[String],
    ) -> impl Future<Output = anyhow::Result<HashMap<String, String>>> + Send;

    fn error_for_input_derivation_without_readable_drv_or_stored_verified_hash(
        &self,
        input: &str,
        dependent: &str,
    ) -> anyhow::Error;
}

/// Inputs that the check did not report come from database rows. Later steps never read them again.
/// Only inputs without a stored value go to the Reader. Reported files are the inputs on the
/// evaluation route. Client-copied files are the inputs on the SSH route.
pub async fn read_transitive_input_derivations_checking_input_addressed_output_paths<
    S: DrvSource,
>(
    source: &S,
    roots: &[String],
) -> anyhow::Result<
    DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath<S::Drv>,
> {
    let roots = normalize_store_paths(roots)?;
    let mut seen: HashSet<String> = roots.iter().cloned().collect();
    let mut frontier: Vec<(String, Option<String>)> =
        roots.into_iter().map(|root| (root, None)).collect();
    let mut found = Vec::new();
    let mut originals = HashMap::new();
    let mut derivation_client_store_path_to_stored_path_when_equal = HashMap::new();
    let mut database_hash_modulo_by_unreported_input_drv_path = HashMap::new();
    while !frontier.is_empty() {
        let read: Vec<_> = futures::stream::iter(std::mem::take(&mut frontier))
            .map(|(path, dependent)| async move { (source.read(&path).await, path, dependent) })
            .buffer_unordered(CONCURRENT_READS)
            .collect()
            .await;

        let mut unreported_input_and_dependent_pairs = Vec::new();
        for (read, path, dependent) in read {
            let read = read?.ok_or_else(|| match &dependent {
                None => anyhow::anyhow!("{path} is not in the project caches, copy it first"),
                Some(dependent) => source
                    .error_for_input_derivation_without_readable_drv_or_stored_verified_hash(
                        &path, dependent,
                    ),
            })?;
            for input in normalize_store_paths(&read.stored_derivation().dependencies)? {
                if seen.insert(input.clone()) {
                    unreported_input_and_dependent_pairs.push((input, path.clone()));
                }
            }

            let original = read.original_drv_store_path()?;
            if original.to_string() == StorePath::parse_stored(&path)?.base() {
                derivation_client_store_path_to_stored_path_when_equal
                    .insert(original.clone(), path.clone());
            }
            originals.insert(original, read.derivation_with_original_paths()?);
            found.push((path, read));
        }

        for chunk in unreported_input_and_dependent_pairs
            .chunks(INPUT_DRV_STORE_PATH_COUNT_PER_STORED_HASH_MODULO_DATABASE_QUERY)
        {
            let drv_paths: Vec<String> = chunk.iter().map(|(input, _)| input.clone()).collect();
            let mut stored = source
                .read_stored_derivation_hash_modulo_text_for_drv_paths(&drv_paths)
                .await?;
            for (input, dependent) in chunk {
                let Some(text) = stored.remove(input) else {
                    frontier.push((input.clone(), Some(dependent.clone())));
                    continue;
                };
                let path = input;
                let hash = hash_modulo_from_prefixed_storage_text(&text).ok_or_else(|| {
                    anyhow::anyhow!(
                        "The stored hash modulo {text} in derivation row {path} won't parse."
                    )
                })?;
                let base = StorePath::parse_stored(input)?.base();
                let client = ClientPath::from_base_path(&base)
                    .map_err(|e| anyhow::anyhow!("{base}: {e}"))?;
                database_hash_modulo_by_unreported_input_drv_path.insert(client, hash);
            }
        }
    }

    let verified_hash_modulo_by_stored_derivation_path_for_equal_client_and_stored_paths =
        check_input_addressed_output_paths(
            &originals,
            &database_hash_modulo_by_unreported_input_drv_path,
        )?
        .into_iter()
        .filter_map(|(original, hash)| {
            let stored =
                derivation_client_store_path_to_stored_path_when_equal.remove(&original)?;
            Some((
                stored,
                VerifiedDerivationHashModulo(hash_modulo_to_prefixed_storage_text(&hash)),
            ))
        })
        .collect();
    Ok(
        DerivationClosureCheckReadFilesAndHashModulosForClientPathEqualToStoredPath {
            read_stored_derivation_path_and_derivation_file_record_pairs: found,
            verified_hash_modulo_by_stored_derivation_path_for_equal_client_and_stored_paths,
        },
    )
}

pub fn normalize_store_paths(paths: &[String]) -> anyhow::Result<Vec<String>> {
    paths
        .iter()
        .map(|path| Ok(StorePath::parse_stored(path)?.full()))
        .collect()
}

pub fn check_input_addressed_output_paths(
    derivations: &HashMap<ClientPath, OriginalDerivation>,
    database_hash_modulo_by_unreported_input_drv_path: &HashMap<ClientPath, HashModulo>,
) -> anyhow::Result<HashMap<ClientPath, HashModulo>> {
    let store_dir = StoreDir::default();
    let mut hashes: HashMap<&ClientPath, HashModulo> = HashMap::new();
    let mut entered: HashSet<&ClientPath> = HashSet::new();
    for root in derivations.keys() {
        let mut pending = vec![root];
        while let Some(&path) = pending.last() {
            if hashes.contains_key(path) {
                pending.pop();
                continue;
            }
            let drv = &derivations[path];
            if entered.insert(path) {
                let inputs = DerivationInputs::from(&drv.inputs);
                pending.extend(
                    inputs
                        .drvs
                        .keys()
                        .filter_map(|input| derivations.get_key_value(input))
                        .map(|(input, _)| input)
                        .filter(|input| !hashes.contains_key(input)),
                );
                continue;
            }

            let lookup = |input: &ClientPath| {
                Ok::<_, Infallible>(
                    hashes
                        .get(input)
                        .or_else(|| database_hash_modulo_by_unreported_input_drv_path.get(input))
                        .cloned()
                        .unwrap_or(HashModulo::DeferredDrv),
                )
            };
            let input_addressed = drv
                .outputs
                .values()
                .any(|output| matches!(output, OriginalOutput::InputAddressed(_)));
            if input_addressed {
                let drv_hash = hash_modulo(&store_dir, drv, lookup)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Can't check the output path of {path}. The input derivation has no known output path."
                    )
                })?;
                for (output, stated) in &drv.outputs {
                    if let OriginalOutput::InputAddressed(stated) = stated {
                        let computed = make_output_path(&store_dir, &drv.name, &drv_hash, output)?;
                        anyhow::ensure!(
                            *stated == computed,
                            "{path} shows output {output} at {stated}, but its inputs give {computed}."
                        );
                    }
                }
            }
            let hash = hash_input_modulo(&store_dir, drv, lookup)?;
            hashes.insert(path, hash);
            pending.pop();
        }
    }
    Ok(hashes
        .into_iter()
        .map(|(path, hash)| (path.clone(), hash))
        .collect())
}
