/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::admit::{admit, admit_nar};
use super::refusal::{CacheRefusal, ensure_cache_enabled};
use crate::api_spec::{Binary, CachePath};
use crate::audit::Audit;
use crate::authorization::RateLimitBucketOwner;
use crate::endpoints::ChunkReceived;
use crate::error::{WebError, WebResult};
use crate::extractors::{JsonBody, Query};
use crate::helpers::ok_json;
use crate::scope::{CacheScope, Requires, can};
use anyhow::Context;
use axum::Extension;
use axum::body::Bytes;
use axum::extract::{Multipart, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_entity::store_path_content_producer::CacheUploadChannel;
use gradient_graph::Graph;
use gradient_proto::import::{ImportInput, StoredContentProducer};
use gradient_storage::PartialStore;
use gradient_storage::admission::HeldPermit;
use gradient_types::checked_uploader_stated_store_path_metadata::CheckedUploadedStorePathReferencesDeriverAndContentAddress;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::events::cache::NarSigned;
use gradient_types::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Serialize, utoipa::ToSchema)]
pub struct StoredNar {
    pub store_path: String,
    pub created: bool,
}

#[derive(utoipa::ToSchema)]
pub struct NarUploadForm {
    pub narinfo: NarinfoPart,
    #[schema(value_type = String, format = Binary)]
    pub nar: Vec<u8>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct NarinfoPart {
    store_path: String,
    file_hash: String,
    file_size: i64,
    nar_size: i64,
    nar_hash: String,
    #[serde(default)]
    references: Vec<String>,
    #[serde(default)]
    deriver: Option<String>,
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/nars",
    operation_id = "uploadCacheNar",
    tag = "caches",
    summary = "Upload a NAR to a cache",
    description = "Uploads a single NAR (its bytes plus narinfo metadata) to this cache.\nRequires `writeStore`. The server validates the byte length against the\ndeclared `file_size`, stores the blob, records the cached path, and\nenqueues a signature for this cache. Re-uploading an existing store path\nreuses the cached path and only adds this cache's signature.\n\nThe route's body limit is `GRADIENT_NAR_MAX_UPLOAD_SIZE` (default 512 MiB).\nThe bundled reverse proxy additionally caps each request body at 100 MiB,\nso NARs above that must use the chunked endpoints\n(`PUT .../nars/{hash}/chunk` + `POST .../nars/{hash}/finalize`).",
    params(CachePath),
    request_body(content = NarUploadForm, content_type = "multipart/form-data"),
    responses((status = CREATED, description = "NAR stored", body = StoredNar), WebError)
)]
pub async fn nars_upload(
    CacheScope { cache, user, .. }: CacheScope<Requires<can::WriteStore>>,
    credential: Option<Extension<RateLimitBucketOwner>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> WebResult<impl IntoResponse> {
    let cache = ensure_cache_enabled(cache)?;
    let max = u64::try_from(state.config.nar.max_upload_size.get()).unwrap_or(u64::MAX);
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok())
        .unwrap_or(max);
    let permit = admit(
        &state.config,
        &state.memory.upload_admission,
        declared.min(max),
    )
    .await?;
    let upload_store = upload_partial_store(&state.config)?;
    let stage_key = format!("{}/oneshot-{}", cache.id, uuid::Uuid::now_v7());

    let result = async {
        let upload = read_multipart_form_stage_nar_to_temp_file_and_check_size(
            &state.cache_mapper(cache.id),
            &upload_store,
            &stage_key,
            &mut multipart,
            max,
        )
        .await?;
        drop(permit);
        let created = verify_admit_import_sign_and_publish_staged_nar(
            &state.db(),
            admit_nar(
                &state.config,
                &state.memory.upload_admission,
                &upload.stored_hash,
                upload.file_size,
            ),
            &state.graph,
            build_rest_upload_producer_record(cache.id, user.id, credential),
            &upload_store,
            &stage_key,
            &upload,
        )
        .await?;
        WebResult::Ok((upload, created))
    }
    .await;
    let _ = upload_store.discard(&stage_key).await;
    let (upload, created) = result?;

    audit.write(Action::CacheNarUpload, EventOwner::cache(cache.id), json!({ "cache_id": cache.id.to_string(), "cache_name": cache.name, "store_path": upload.client_path, "created": created, }))
    .await;

    Ok((
        StatusCode::CREATED,
        ok_json(StoredNar {
            store_path: upload.client_path,
            created,
        }),
    ))
}

async fn read_multipart_form_stage_nar_to_temp_file_and_check_size(
    mapper: &StorePathMapper,
    upload_store: &PartialStore,
    stage_key: &str,
    multipart: &mut Multipart,
    max: u64,
) -> WebResult<MappedUpload> {
    let mut narinfo: Option<NarinfoPart> = None;
    let mut staged_size: Option<u64> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| CacheRefusal::MultipartFieldUnreadable(e.to_string()))?
    {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("narinfo") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| CacheRefusal::MultipartFieldUnreadable(e.to_string()))?;
                let parsed: NarinfoPart =
                    serde_json::from_str(&text).map_err(|e| CacheRefusal::InvalidNarinfoJson {
                        reason: e.to_string(),
                    })?;
                narinfo = Some(parsed);
            }
            Some("nar") => {
                staged_size = Some(stage_nar_field(upload_store, stage_key, field, max).await?);
            }
            _ => {}
        }
    }

    let narinfo = narinfo.ok_or(CacheRefusal::MissingPart("narinfo"))?;
    let upload = MappedUpload::new(narinfo, mapper)?;
    let staged_size = staged_size.ok_or(CacheRefusal::MissingPart("nar"))?;
    if staged_size != upload.file_size {
        return Err(CacheRefusal::UploadedNarSizeMismatch {
            declared: upload.narinfo.file_size,
            got: staged_size,
        }
        .into());
    }
    Ok(upload)
}

async fn verify_admit_import_sign_and_publish_staged_nar(
    db: &DbContext,
    admission: impl Future<Output = WebResult<HeldPermit>>,
    graph: &Graph,
    producer: StoredContentProducer,
    store: &PartialStore,
    key: &str,
    upload: &MappedUpload,
) -> WebResult<bool> {
    // A chunk request reaches a staging file only through its shared name. Once detached, the
    // bytes are copied into a storage object nobody else can write and that object is verified.
    let staged = store
        .detach(key)
        .await
        .context("Detachment of the Staged NAR")?
        .ok_or(CacheRefusal::StagedNarSizeMismatch {
            declared: upload.narinfo.file_size,
            staged: 0,
        })?;
    let nar_storage = &db.storage.nar_storage;
    let temporary = match store.open_read(&staged).await {
        Ok(reader) => {
            nar_storage
                .stream_reader_to_temporary_upload(&upload.stored_hash, reader)
                .await
        }
        Err(e) => Err(e),
    };
    let _ = store.discard(&staged).await;
    let temporary = temporary.context("copy the staged NAR to a temporary storage object")?;

    let placed = verify_temporary_upload_wait_for_admission_and_move_to_variant_storage_key(
        db, admission, &temporary, upload,
    )
    .await;
    if placed.is_err() {
        let _ = nar_storage
            .delete_temporary_nar_upload(temporary.as_str())
            .await;
    }
    let (verified, claim) = placed?;

    let outcome = graph
        .commit_nar(upload.import_input(&verified).to_commit(producer))
        .await
        .map_err(WebError::from)?;
    drop(claim);

    for cache in outcome.signed {
        db.events.publish(NarSigned {
            cache,
            hash: upload.original_hash.clone(),
        });
    }
    Ok(outcome.created)
}

async fn verify_temporary_upload_wait_for_admission_and_move_to_variant_storage_key(
    db: &DbContext,
    admission: impl Future<Output = WebResult<HeldPermit>>,
    temporary: &gradient_storage::ServerWrittenTemporaryNarKey,
    upload: &MappedUpload,
) -> WebResult<(
    gradient_storage::ServerComputedNarAndCompressedFileHashesAndSizes,
    HeldPermit,
)> {
    let nar_storage = &db.storage.nar_storage;
    let narinfo = &upload.narinfo;
    let verified = nar_storage
        .read_uploaded_nar_at_temporary_key_and_check_client_hash_and_sizes(
            temporary,
            &gradient_storage::ClientClaimedNarUploadHashesAndSizes {
                file_hash: &narinfo.file_hash,
                file_size: upload.file_size,
                nar_hash: &narinfo.nar_hash,
                nar_size: narinfo.nar_size.unsigned_abs(),
                claimed_store_path_references: &upload.claimed_store_path_references,
                uploaded_nar_with_stated_content_address_to_check: Some(
                    gradient_storage::UploadedStorePathAndContentAddress {
                        store_path: &upload.client_path,
                        content_address: None,
                    },
                ),
            },
        )
        .await
        .map_err(|e| CacheRefusal::NarVerificationFailed {
            reason: e.to_string(),
        })?;

    let claim = admission.await?;
    let key = verified
        .object_storage_key_for_store_path_hash_and_compressed_file_sha256(&upload.stored_hash);
    if nar_storage.exists(&key).await? {
        nar_storage
            .delete_temporary_nar_upload(temporary.as_str())
            .await?;
    } else {
        nar_storage
            .move_verified_upload_to_final_storage_key(&upload.stored_hash, temporary, &verified)
            .await?;
    }
    Ok((verified, claim))
}

struct MappedUpload {
    client_path: String,
    original_hash: String,
    stored_hash: String,
    file_size: u64,
    claimed_store_path_references: Vec<String>,
    stated: CheckedUploadedStorePathReferencesDeriverAndContentAddress,
    narinfo: NarinfoPart,
}

impl MappedUpload {
    fn new(mut narinfo: NarinfoPart, mapper: &StorePathMapper) -> WebResult<Self> {
        let file_size =
            u64::try_from(narinfo.file_size).map_err(|_| CacheRefusal::NegativeNarInfoSize)?;
        if narinfo.nar_size < 0 {
            return Err(CacheRefusal::NegativeNarInfoSize.into());
        }
        let original = OriginalStorePath::parse(&narinfo.store_path)
            .map_err(|_| CacheRefusal::InvalidStoreHash)?;
        let client_stated = CheckedUploadedStorePathReferencesDeriverAndContentAddress::checked_from_references_deriver_and_content_address(
            std::mem::take(&mut narinfo.references),
            narinfo.deriver.take(),
            None,
        )?;
        let claimed_store_path_references = client_stated.references().to_vec();
        let stated = client_stated.rewrite_reference_and_deriver_hashes_to_stored_hashes(mapper)?;
        let client_path = narinfo.store_path.clone();
        narinfo.store_path = mapper
            .inbound_text_if_exact_store_path(&narinfo.store_path)
            .map_err(|_| CacheRefusal::InvalidStoreHash)?;
        Ok(Self {
            client_path,
            original_hash: original.hash().to_string(),
            stored_hash: mapper.inbound_hash(original.hash().clone()).to_string(),
            file_size,
            claimed_store_path_references,
            stated,
            narinfo,
        })
    }

    fn import_input<'a>(
        &'a self,
        verified: &'a gradient_storage::ServerComputedNarAndCompressedFileHashesAndSizes,
    ) -> ImportInput<'a> {
        ImportInput {
            store_path: &self.narinfo.store_path,
            verified,
            references: self.stated.references(),
            deriver: self.stated.deriver(),
            ca: self.stated.checked_content_address_text(),
        }
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NarChunkQuery {
    offset: u64,
}

fn build_rest_upload_producer_record(
    cache: CacheId,
    uploader: UserId,
    credential: Option<Extension<RateLimitBucketOwner>>,
) -> StoredContentProducer {
    StoredContentProducer::Upload {
        caches: vec![cache],
        user: Some(uploader),
        api_key: match credential {
            Some(Extension(RateLimitBucketOwner::ApiKey(key))) => Some(key),
            _ => None,
        },
        channel: CacheUploadChannel::RestHttpApiUpload,
    }
}

fn upload_partial_store(config: &RuntimeConfig) -> WebResult<PartialStore> {
    Ok(PartialStore::new(config.server.nar_upload_partial_dir())?)
}

async fn stage_nar_field(
    store: &PartialStore,
    key: &str,
    mut field: axum::extract::multipart::Field<'_>,
    max: u64,
) -> WebResult<u64> {
    let token = "oneshot";
    let mut offset = 0u64;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| CacheRefusal::MultipartFieldUnreadable(e.to_string()))?
    {
        if offset + chunk.len() as u64 > max {
            return Err(CacheRefusal::NarTooLarge { max }.into());
        }
        store
            .append(key, token, offset, &chunk)
            .await
            .context("stage NAR")?;
        offset += chunk.len() as u64;
    }
    Ok(offset)
}

fn parse_upload_path_to_store_hash(segment: &str, mapper: &StorePathMapper) -> WebResult<String> {
    let original_hash = StoreHash::parse(segment)
        .or_else(|_| OriginalStorePath::parse(segment).map(|path| path.hash().clone()))
        .map_err(|_| CacheRefusal::InvalidStoreHash)?;
    Ok(mapper.inbound_hash(original_hash).to_string())
}

#[utoipa::path(
    put,
    path = "/caches/{cache}/nars/{hash}/chunk",
    operation_id = "uploadCacheNarChunk",
    tag = "caches",
    summary = "Append a chunk of a NAR being uploaded",
    description = "Appends one slice of a NAR to a server-side `.partial` staging file,\none per `(cache, hash)`. Used to upload NARs larger than the reverse\nproxy's per-request body limit by splitting them across several requests.\nRequires `writeStore`. `offset == 0` starts fresh (truncating any stale\npartial); otherwise the chunk is appended only when `offset` equals the\nbytes already staged. The response's `received` is authoritative: the\nclient sets its next offset to it (resending if it did not advance) and\ncalls `POST .../finalize` once `received` equals the NAR size.",
    params(CachePath, ("hash" = String, Path), NarChunkQuery),
    request_body(content = Binary, content_type = "application/octet-stream"),
    responses((status = OK, description = "Chunk staged", body = ChunkReceived), WebError)
)]
pub async fn nar_chunk(
    CacheScope { cache, params, .. }: CacheScope<Requires<can::WriteStore>>,
    state: State<Arc<ServerState>>,
    Query(NarChunkQuery { offset }): Query<NarChunkQuery>,
    body: Bytes,
) -> WebResult<impl IntoResponse> {
    let cache = ensure_cache_enabled(cache)?;
    let store_hash = parse_upload_path_to_store_hash(
        &params.param::<String>("hash")?,
        &state.cache_mapper(cache.id),
    )?;

    let max = u64::try_from(state.config.nar.max_upload_size.get()).unwrap_or(u64::MAX);
    let end = offset
        .checked_add(body.len() as u64)
        .filter(|end| *end <= max)
        .ok_or(CacheRefusal::NarTooLarge { max })?;

    let store = upload_partial_store(&state.config)?;
    let key = format!("{}/{store_hash}", cache.id);
    // An `offset == 0` append is truncating any stale prefix. Abandoned partials are left to the
    // deep GC.

    // `received` is authoritative. A non-contiguous `offset` is appending nothing, and the current
    // length is reported for the caller to resync.
    let staged = store.received_len(&key, &store_hash).await?;
    let received = if offset == 0 || offset == staged {
        let permit = admit(
            &state.config,
            &state.memory.upload_admission,
            body.len() as u64,
        )
        .await?;
        store.append(&key, &store_hash, offset, &body).await?;
        drop(permit);
        end
    } else {
        staged
    };

    Ok((StatusCode::OK, ok_json(ChunkReceived { received })))
}

#[utoipa::path(
    post,
    path = "/caches/{cache}/nars/{hash}/finalize",
    operation_id = "finalizeCacheNarUpload",
    tag = "caches",
    summary = "Finalize a chunked NAR upload",
    description = "Validates the fully staged NAR against its narinfo (the staged byte\nlength must equal `file_size`), imports it exactly like the single-shot\nupload, and drops the `.partial`. Requires `writeStore`.",
    params(CachePath, ("hash" = String, Path)),
    request_body = NarinfoPart,
    responses((status = CREATED, description = "NAR stored", body = StoredNar), WebError)
)]
pub async fn nar_finalize(
    CacheScope {
        cache,
        user,
        params,
        ..
    }: CacheScope<Requires<can::WriteStore>>,
    credential: Option<Extension<RateLimitBucketOwner>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    JsonBody(narinfo): JsonBody<NarinfoPart>,
) -> WebResult<impl IntoResponse> {
    let cache = ensure_cache_enabled(cache)?;
    let mapper = state.cache_mapper(cache.id);
    let store_hash = parse_upload_path_to_store_hash(&params.param::<String>("hash")?, &mapper)?;
    let upload = MappedUpload::new(narinfo, &mapper)?;
    if store_hash != upload.stored_hash {
        return Err(CacheRefusal::UrlPathStoreHashNarInfoStorePathHashMismatch.into());
    }

    let store = upload_partial_store(&state.config)?;
    let key = format!("{}/{store_hash}", cache.id);
    let staged = store.staged_len(&key).await;
    if staged != upload.file_size {
        return Err(CacheRefusal::StagedNarSizeMismatch {
            declared: upload.narinfo.file_size,
            staged,
        }
        .into());
    }

    let created = verify_admit_import_sign_and_publish_staged_nar(
        &state.db(),
        admit_nar(
            &state.config,
            &state.memory.upload_admission,
            &upload.stored_hash,
            upload.file_size,
        ),
        &state.graph,
        build_rest_upload_producer_record(cache.id, user.id, credential),
        &store,
        &key,
        &upload,
    )
    .await?;

    audit.write(Action::CacheNarUpload, EventOwner::cache(cache.id), json!({ "cache_id": cache.id.to_string(), "cache_name": cache.name, "store_path": upload.client_path, "created": created, "chunked": true, }))
    .await;

    Ok((
        StatusCode::CREATED,
        ok_json(StoredNar {
            store_path: upload.client_path,
            created,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::{MappedUpload, NarinfoPart, parse_upload_path_to_store_hash};
    use gradient_entity::{
        EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, IdentityMapping, RotatedHashMapping,
    };
    use gradient_types::{CacheId, StorePathMapper};
    use std::sync::Arc;

    fn part(store_path: String, references: Vec<String>) -> NarinfoPart {
        NarinfoPart {
            store_path,
            file_hash: "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into(),
            file_size: 5,
            nar_size: 5,
            nar_hash: "sha256:0mdqa9w1p6cmli6976v4wi0sw9r4p5prkj7lzfd1877wk11c9c73".into(),
            references,
            deriver: None,
        }
    }

    fn rotated() -> StorePathMapper {
        StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil())
    }

    fn narinfo_part_for_bytes_with_matching_hashes(compressed: &[u8], nar: &[u8]) -> NarinfoPart {
        use sha2::Digest as _;
        NarinfoPart {
            file_hash: gradient_storage::file_hash_sri(compressed),
            file_size: compressed.len() as i64,
            nar_size: nar.len() as i64,
            nar_hash: format!("sha256:{}", hex::encode(sha2::Sha256::digest(nar))),
            ..part(
                format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello"),
                Vec::new(),
            )
        }
    }

    fn rest_http_api_upload_producer_with_nil_cache_id_and_no_user_or_api_key()
    -> gradient_proto::import::StoredContentProducer {
        gradient_proto::import::StoredContentProducer::Upload {
            caches: vec![CacheId::nil()],
            user: None,
            api_key: None,
            channel: super::CacheUploadChannel::RestHttpApiUpload,
        }
    }

    fn test_server_state_with_empty_database_and_local_nar_storage(
        dir: &std::path::Path,
    ) -> Arc<gradient_core::ServerState> {
        gradient_test_support::state::TestState::new()
            .web_db(gradient_fixtures::mock::empty_db())
            .nar_storage(gradient_storage::NarStore::local(dir).unwrap())
            .build()
    }

    fn collect_regular_file_paths_recursively(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .flat_map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    collect_regular_file_paths_recursively(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn rejects_rest_upload_when_zstd_nar_decodes_to_bytes_not_matching_claimed_nar_hash() {
        let nar_dir = tempfile::TempDir::new().unwrap();
        let state = test_server_state_with_empty_database_and_local_nar_storage(nar_dir.path());
        let compressed = zstd::encode_all(&b"nar X"[..], 3).unwrap();
        let upload = MappedUpload::new(
            narinfo_part_for_bytes_with_matching_hashes(&compressed, b"nar Y"),
            &rotated(),
        )
        .unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let store = gradient_storage::PartialStore::new(dir.path()).unwrap();
        store.append("stage", "t", 0, &compressed).await.unwrap();

        let refused = super::verify_admit_import_sign_and_publish_staged_nar(
            &state.db(),
            super::admit_nar(
                &state.config,
                &state.memory.upload_admission,
                &upload.stored_hash,
                upload.file_size,
            ),
            &state.graph,
            rest_http_api_upload_producer_with_nil_cache_id_and_no_user_or_api_key(),
            &store,
            "stage",
            &upload,
        )
        .await;

        let Err(error) = refused else {
            panic!("Reject a byte sequence of NAR X with the declared hash of NAR Y.")
        };
        assert!(
            format!("{error:?}").contains("Decoded NAR hash does not match"),
            "{error:?}"
        );
        assert_eq!(
            collect_regular_file_paths_recursively(nar_dir.path()),
            Vec::<std::path::PathBuf>::new()
        );
    }

    #[tokio::test]
    async fn stored_nar_keeps_original_bytes_when_staging_file_is_overwritten_during_admission_wait()
     {
        let nar_dir = tempfile::TempDir::new().unwrap();
        let state = test_server_state_with_empty_database_and_local_nar_storage(nar_dir.path());
        let nar = b"nar of the legitimate build";
        let compressed = zstd::encode_all(&nar[..], 3).unwrap();
        let replacement = zstd::encode_all(&b"replacement bytes"[..], 3).unwrap();
        let upload = MappedUpload::new(
            narinfo_part_for_bytes_with_matching_hashes(&compressed, nar),
            &rotated(),
        )
        .unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let store = gradient_storage::PartialStore::new(dir.path()).unwrap();
        store.append("stage", "t", 0, &compressed).await.unwrap();

        let write_replacement_chunk_then_wait_for_admission_future = async {
            store.append("stage", "t", 0, &replacement).await.unwrap();
            for staged in collect_regular_file_paths_recursively(dir.path()) {
                std::fs::write(staged, &replacement).unwrap();
            }
            super::admit_nar(
                &state.config,
                &state.memory.upload_admission,
                &upload.stored_hash,
                upload.file_size,
            )
            .await
        };
        super::verify_admit_import_sign_and_publish_staged_nar(
            &state.db(),
            write_replacement_chunk_then_wait_for_admission_future,
            &state.graph,
            rest_http_api_upload_producer_with_nil_cache_id_and_no_user_or_api_key(),
            &store,
            "stage",
            &upload,
        )
        .await
        .unwrap();

        let key = gradient_storage::verify_zstd_nar_stream_sha256_and_sizes_against_claims(
            compressed.as_slice(),
            &gradient_storage::ClientClaimedNarUploadHashesAndSizes {
                file_hash: &upload.narinfo.file_hash,
                file_size: upload.file_size,
                nar_hash: &upload.narinfo.nar_hash,
                nar_size: nar.len() as u64,
                claimed_store_path_references: &[],
                uploaded_nar_with_stated_content_address_to_check: None,
            },
        )
        .await
        .unwrap()
        .object_storage_key_for_store_path_hash_and_compressed_file_sha256(&upload.stored_hash);
        let stored = state.db().storage.nar_storage.get(&key).await.unwrap();
        assert_eq!(stored, Some(compressed));
    }

    const OTHER_EXAMPLE_STORE_PATH_HASH_BEFORE_CACHE_REWRITE: &str =
        "abcdfghijklmnpqrsvwxyz0123456789";

    fn uploaded_nar_store_path() -> String {
        format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello")
    }

    fn other_path() -> String {
        format!("/nix/store/{OTHER_EXAMPLE_STORE_PATH_HASH_BEFORE_CACHE_REWRITE}-bash")
    }

    fn identity() -> StorePathMapper {
        StorePathMapper::for_cache(Arc::new(IdentityMapping), CacheId::nil())
    }

    async fn run_staged_rest_nar_upload(
        nar_dir: &std::path::Path,
        store_path: String,
        nar: &[u8],
        references: Vec<String>,
        deriver: Option<String>,
        mapper: &StorePathMapper,
    ) -> crate::error::WebResult<bool> {
        let state = test_server_state_with_empty_database_and_local_nar_storage(nar_dir);
        let compressed = zstd::encode_all(nar, 3).unwrap();
        let upload = MappedUpload::new(
            NarinfoPart {
                store_path,
                references,
                deriver,
                ..narinfo_part_for_bytes_with_matching_hashes(&compressed, nar)
            },
            mapper,
        )?;
        let dir = tempfile::TempDir::new().unwrap();
        let store = gradient_storage::PartialStore::new(dir.path()).unwrap();
        store.append("stage", "t", 0, &compressed).await.unwrap();
        super::verify_admit_import_sign_and_publish_staged_nar(
            &state.db(),
            super::admit_nar(
                &state.config,
                &state.memory.upload_admission,
                &upload.stored_hash,
                upload.file_size,
            ),
            &state.graph,
            rest_http_api_upload_producer_with_nil_cache_id_and_no_user_or_api_key(),
            &store,
            "stage",
            &upload,
        )
        .await
    }

    #[tokio::test]
    async fn cache_upload_rejects_reference_hash_missing_from_nar_bytes_with_bad_request_and_no_stored_nar()
     {
        use axum::response::IntoResponse as _;
        let nar_dir = tempfile::TempDir::new().unwrap();
        let nar = format!("nar of hello linked to {}", uploaded_nar_store_path());

        let refused = run_staged_rest_nar_upload(
            nar_dir.path(),
            uploaded_nar_store_path(),
            nar.as_bytes(),
            vec![uploaded_nar_store_path(), other_path()],
            None,
            &identity(),
        )
        .await;

        let Err(error) = refused else {
            panic!("Refuse a claimed reference that the NAR does not contain.")
        };
        assert!(format!("{error:?}").contains(&other_path()), "{error:?}");
        assert_eq!(
            error.into_response().status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        assert_eq!(
            collect_regular_file_paths_recursively(nar_dir.path()),
            Vec::<std::path::PathBuf>::new()
        );
    }

    #[tokio::test]
    async fn nar_upload_with_all_claimed_references_present_is_stored_with_identity_mapping() {
        let nar_dir = tempfile::TempDir::new().unwrap();
        let nar = format!(
            "nar of hello linked to {} and {}",
            other_path(),
            uploaded_nar_store_path()
        );

        run_staged_rest_nar_upload(
            nar_dir.path(),
            uploaded_nar_store_path(),
            nar.as_bytes(),
            vec![other_path(), uploaded_nar_store_path()],
            None,
            &identity(),
        )
        .await
        .unwrap();

        assert!(!collect_regular_file_paths_recursively(nar_dir.path()).is_empty());
    }

    #[tokio::test]
    async fn upload_is_stored_when_store_path_hash_mapping_changes_hashes() {
        let nar_dir = tempfile::TempDir::new().unwrap();
        let nar = format!(
            "nar of hello linked to {} and {}",
            other_path(),
            uploaded_nar_store_path()
        );

        run_staged_rest_nar_upload(
            nar_dir.path(),
            uploaded_nar_store_path(),
            nar.as_bytes(),
            vec![other_path(), uploaded_nar_store_path()],
            Some(format!(
                "{OTHER_EXAMPLE_STORE_PATH_HASH_BEFORE_CACHE_REWRITE}-hello.drv"
            )),
            &rotated(),
        )
        .await
        .unwrap();

        assert!(!collect_regular_file_paths_recursively(nar_dir.path()).is_empty());
    }

    #[tokio::test]
    async fn upload_rejects_invalid_store_path_reference_and_deriver_fields_with_400_and_no_stored_nar()
     {
        use axum::response::IntoResponse as _;
        let line_feed_path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello\nInjected: 1");
        let drv = format!("{EXAMPLE_ORIGINAL_HASH}-hello.drv");
        let cases = [
            (
                "store path name with a line feed",
                Some(line_feed_path.clone()),
                vec![],
                None,
            ),
            (
                "reference with a line feed",
                None,
                vec![line_feed_path.clone()],
                None,
            ),
            (
                "reference that is no store path",
                None,
                vec!["hello".to_owned()],
                None,
            ),
            (
                "deriver with a line feed",
                None,
                vec![],
                Some(format!("{drv}\nInjected: 1")),
            ),
            (
                "deriver that is no .drv",
                None,
                vec![],
                Some(uploaded_nar_store_path()),
            ),
        ];
        let mut accepted = Vec::new();
        for (case, store_path, references, deriver) in cases {
            let nar_dir = tempfile::TempDir::new().unwrap();
            let nar = format!("nar of hello linked to {}", uploaded_nar_store_path());
            let compressed = zstd::encode_all(nar.as_bytes(), 3).unwrap();
            let narinfo = NarinfoPart {
                store_path: store_path.unwrap_or_else(uploaded_nar_store_path),
                references,
                deriver,
                ..narinfo_part_for_bytes_with_matching_hashes(&compressed, nar.as_bytes())
            };
            let outcome = match MappedUpload::new(narinfo, &identity()) {
                Err(error) => Err(error),
                Ok(upload) => {
                    run_staged_rest_nar_upload(
                        nar_dir.path(),
                        uploaded_nar_store_path(),
                        nar.as_bytes(),
                        upload.claimed_store_path_references.clone(),
                        upload.stated.deriver().map(str::to_owned),
                        &identity(),
                    )
                    .await
                }
            };
            match outcome {
                Err(error) => {
                    assert_eq!(
                        error.into_response().status(),
                        axum::http::StatusCode::BAD_REQUEST,
                        "{case}"
                    );
                    assert_eq!(
                        collect_regular_file_paths_recursively(nar_dir.path()),
                        Vec::<std::path::PathBuf>::new()
                    );
                }
                Ok(_) => accepted.push(case),
            }
        }
        assert_eq!(accepted, Vec::<&str>::new());
    }

    #[tokio::test]
    async fn rest_upload_rejects_drv_whose_text_does_not_produce_uploaded_store_path_and_stores_no_nar()
     {
        use axum::response::IntoResponse as _;
        let nar_dir = tempfile::TempDir::new().unwrap();
        let changed = include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv")
            .replace("x86_64-linux", "aarch64-linux");
        let input = include_str!("../../../../gradient-ssh/testdata/input-addressed-dep.drv.path");

        let refused = run_staged_rest_nar_upload(
            nar_dir.path(),
            include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv.path")
                .trim()
                .to_owned(),
            &gradient_util::nar::single_file_nar(changed.as_bytes(), false),
            vec![input.trim().to_owned()],
            None,
            &identity(),
        )
        .await;

        let Err(error) = refused else {
            panic!(
                "Reject a derivation whose store path does not match the one computed from its text."
            )
        };
        assert_eq!(
            error.into_response().status(),
            axum::http::StatusCode::BAD_REQUEST
        );
        assert_eq!(
            collect_regular_file_paths_recursively(nar_dir.path()),
            Vec::<std::path::PathBuf>::new()
        );
    }

    #[tokio::test]
    async fn test_rest_cache_upload_stores_drv_at_path_derived_from_text_and_references() {
        let nar_dir = tempfile::TempDir::new().unwrap();
        let input = include_str!("../../../../gradient-ssh/testdata/input-addressed-dep.drv.path");

        run_staged_rest_nar_upload(
            nar_dir.path(),
            include_str!("../../../../gradient-ssh/testdata/input-addressed-top.drv.path")
                .trim()
                .to_owned(),
            &gradient_util::nar::single_file_nar(
                include_bytes!("../../../../gradient-ssh/testdata/input-addressed-top.drv"),
                false,
            ),
            vec![input.trim().to_owned()],
            None,
            &identity(),
        )
        .await
        .unwrap();

        assert!(!collect_regular_file_paths_recursively(nar_dir.path()).is_empty());
    }

    #[tokio::test]
    async fn rest_cache_upload_real_fixed_output_file_is_stored() {
        let nar_dir = tempfile::TempDir::new().unwrap();
        let contents = b"tarball bytes";
        let fixed_output = gradient_test_support::content_address_and_store_path::flat_fixed_output_store_path_and_content_address_from_file_name_and_bytes(
            "source.tar.gz",
            contents,
        );

        run_staged_rest_nar_upload(
            nar_dir.path(),
            fixed_output.store_path,
            &gradient_util::nar::single_file_nar(contents, false),
            Vec::new(),
            None,
            &identity(),
        )
        .await
        .unwrap();

        assert!(!collect_regular_file_paths_recursively(nar_dir.path()).is_empty());
    }

    #[test]
    fn an_uploaded_narinfo_is_stored_under_its_mapped_hash() {
        let (o, s) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
        let uploaded = NarinfoPart {
            deriver: Some(format!("{o}-hello.drv")),
            ..part(
                format!("/nix/store/{o}-hello"),
                vec![format!("/nix/store/{o}-hello")],
            )
        };

        let stored = MappedUpload::new(uploaded, &rotated()).unwrap();

        assert_eq!(stored.narinfo.store_path, format!("/nix/store/{s}-hello"));
        assert_eq!(
            stored.stated.references(),
            [format!("/nix/store/{s}-hello")]
        );
        assert_eq!(
            stored.stated.deriver(),
            Some(format!("{s}-hello.drv").as_str())
        );
        assert_eq!(
            stored.claimed_store_path_references,
            [format!("/nix/store/{o}-hello")]
        );
        assert_eq!(stored.narinfo.file_size, 5);
    }

    #[test]
    fn an_uploaded_narinfo_with_a_short_hash_is_refused() {
        let o = EXAMPLE_ORIGINAL_HASH;
        assert!(MappedUpload::new(part("abc-hello".into(), Vec::new()), &rotated()).is_err());
        assert!(
            MappedUpload::new(
                part(format!("{o}-hello"), vec!["abc-bash".into()]),
                &rotated()
            )
            .is_err()
        );
    }

    #[test]
    fn a_negative_declared_size_is_refused() {
        let path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        for (file_size, nar_size) in [(-1, 5), (5, -1)] {
            let narinfo = NarinfoPart {
                file_size,
                nar_size,
                ..part(path.clone(), Vec::new())
            };
            assert!(MappedUpload::new(narinfo, &rotated()).is_err());
        }
    }

    #[test]
    fn a_finalize_url_must_name_the_narinfo_store_path() {
        let path = format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello");
        let upload = MappedUpload::new(part(path, Vec::new()), &rotated()).unwrap();

        for segment in [
            EXAMPLE_ORIGINAL_HASH.to_owned(),
            format!("{EXAMPLE_ORIGINAL_HASH}-hello"),
            format!("/nix/store/{EXAMPLE_ORIGINAL_HASH}-hello"),
        ] {
            assert_eq!(
                parse_upload_path_to_store_hash(&segment, &rotated()).unwrap(),
                upload.stored_hash
            );
        }
        assert_ne!(
            parse_upload_path_to_store_hash(EXAMPLE_STORED_HASH, &rotated()).unwrap(),
            upload.stored_hash
        );
    }

    #[test]
    fn accepts_a_normal_store_basename() {
        let segment = format!("{EXAMPLE_ORIGINAL_HASH}-linux-rpi-6.12.75");
        assert!(parse_upload_path_to_store_hash(&segment, &rotated()).is_ok());
    }

    #[test]
    fn rejects_traversal_and_separators() {
        for bad in ["", "../etc/passwd", "a/b", "..", "x/.."] {
            assert!(
                parse_upload_path_to_store_hash(bad, &rotated()).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }
}
