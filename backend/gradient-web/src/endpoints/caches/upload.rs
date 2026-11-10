/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::admit::{admit, admit_nar};
use super::refusal::CacheRefusal;
use crate::api_spec::{Binary, CachePath};
use crate::audit::Audit;
use crate::endpoints::ChunkReceived;
use crate::error::{WebError, WebResult};
use crate::helpers::ok_json;
use crate::scope::{CacheScope, Requires, can};
use anyhow::Context;
use axum::body::Bytes;
use axum::extract::{Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use gradient_core::ServerState;
use gradient_entity::StorePathError;
use gradient_proto::import::{ImportInput, SignTargets, import_nar_reader};
use gradient_storage::PartialStore;
use gradient_storage::admission::HeldPermit;
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
    CacheScope { cache, .. }: CacheScope<Requires<can::WriteStore>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> WebResult<impl IntoResponse> {
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

    let result = read_multipart_form_stage_nar_to_temp_file_and_import(
        &state,
        cache.id,
        &upload_store,
        &stage_key,
        &mut multipart,
        max,
        permit,
    )
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

async fn read_multipart_form_stage_nar_to_temp_file_and_import(
    state: &ServerState,
    cache: CacheId,
    upload_store: &PartialStore,
    stage_key: &str,
    multipart: &mut Multipart,
    max: u64,
    permit: HeldPermit,
) -> WebResult<(MappedUpload, bool)> {
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
    let upload = MappedUpload::new(narinfo, &state.cache_mapper(cache))?;
    let staged_size = staged_size.ok_or(CacheRefusal::MissingPart("nar"))?;
    if staged_size != upload.file_size {
        return Err(CacheRefusal::UploadedNarSizeMismatch {
            declared: upload.narinfo.file_size,
            got: staged_size,
        }
        .into());
    }
    permit.committed();

    let created = verify_admit_import_sign_and_publish_staged_nar(
        state,
        cache,
        upload_store,
        stage_key,
        &upload,
    )
    .await?;
    Ok((upload, created))
}

async fn verify_admit_import_sign_and_publish_staged_nar(
    state: &ServerState,
    cache: CacheId,
    store: &PartialStore,
    key: &str,
    upload: &MappedUpload,
) -> WebResult<bool> {
    let narinfo = &upload.narinfo;
    let verify_reader = store.open_read(key).await.context("read staged NAR")?;
    gradient_storage::verify_nar_reader(verify_reader, &narinfo.file_hash, upload.file_size)
        .await
        .map_err(|e| CacheRefusal::NarVerificationFailed {
            reason: e.to_string(),
        })?;

    let claim = admit_nar(
        &state.config,
        &state.memory.upload_admission,
        &upload.stored_hash,
        upload.file_size,
    )
    .await?;
    let nar_reader = store.open_read(key).await.context("read staged NAR")?;
    let outcome = import_nar_reader(
        &state.web_db,
        &state.nar_storage,
        &state.graph,
        nar_reader,
        upload.import_input(),
        SignTargets::Cache(cache),
    )
    .await
    .map_err(WebError::from)?;
    if let Some(claim) = claim {
        claim.committed();
    }

    for cache in outcome.signed {
        state.events.publish(NarSigned {
            cache,
            hash: upload.original_hash.clone(),
        });
    }
    Ok(outcome.created)
}

fn stored_narinfo(
    part: NarinfoPart,
    mapper: &StorePathMapper,
) -> Result<NarinfoPart, StorePathError> {
    Ok(NarinfoPart {
        store_path: mapper.inbound_text(&part.store_path)?,
        references: part
            .references
            .iter()
            .map(|reference| mapper.inbound_text(reference))
            .collect::<Result<_, _>>()?,
        deriver: part
            .deriver
            .as_deref()
            .map(|deriver| mapper.inbound_text(deriver))
            .transpose()?,
        ..part
    })
}

struct MappedUpload {
    client_path: String,
    original_hash: String,
    stored_hash: String,
    file_size: u64,
    narinfo: NarinfoPart,
}

impl MappedUpload {
    fn new(narinfo: NarinfoPart, mapper: &StorePathMapper) -> WebResult<Self> {
        let file_size =
            u64::try_from(narinfo.file_size).map_err(|_| CacheRefusal::NegativeNarInfoSize)?;
        if narinfo.nar_size < 0 {
            return Err(CacheRefusal::NegativeNarInfoSize.into());
        }
        let original = OriginalStorePath::parse(&narinfo.store_path)
            .map_err(|_| CacheRefusal::InvalidStoreHash)?;
        let client_path = narinfo.store_path.clone();
        let narinfo =
            stored_narinfo(narinfo, mapper).map_err(|_| CacheRefusal::InvalidStoreHash)?;
        Ok(Self {
            client_path,
            original_hash: original.hash().to_string(),
            stored_hash: mapper.inbound_hash(original.hash().clone()).to_string(),
            file_size,
            narinfo,
        })
    }

    fn import_input(&self) -> ImportInput<'_> {
        ImportInput {
            store_path: &self.narinfo.store_path,
            file_hash: &self.narinfo.file_hash,
            file_size: self.narinfo.file_size,
            nar_size: self.narinfo.nar_size,
            nar_hash: &self.narinfo.nar_hash,
            references: &self.narinfo.references,
            deriver: self.narinfo.deriver.as_deref(),
            ca: None,
        }
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NarChunkQuery {
    offset: u64,
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

fn stored_segment(segment: &str, mapper: &StorePathMapper) -> WebResult<String> {
    let stored = mapper
        .inbound_text(segment)
        .map_err(|_| CacheRefusal::InvalidStoreHash)?;
    require_safe_hash(&stored)?;
    Ok(stored)
}

fn segment_names_stored_hash(segment: &str, stored_hash: &str) -> bool {
    segment.split_once('-').map_or(segment, |(hash, _)| hash) == stored_hash
}

fn require_safe_hash(store_hash: &str) -> WebResult<()> {
    if store_hash.is_empty() || store_hash.contains('/') || store_hash.contains("..") {
        return Err(CacheRefusal::InvalidStoreHash.into());
    }

    Ok(())
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
    let store_hash = stored_segment(
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
        permit.committed();
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
    CacheScope { cache, params, .. }: CacheScope<Requires<can::WriteStore>>,
    state: State<Arc<ServerState>>,
    audit: Audit,
    axum::Json(narinfo): axum::Json<NarinfoPart>,
) -> WebResult<impl IntoResponse> {
    let mapper = state.cache_mapper(cache.id);
    let store_hash = stored_segment(&params.param::<String>("hash")?, &mapper)?;
    let upload = MappedUpload::new(narinfo, &mapper)?;
    if !segment_names_stored_hash(&store_hash, &upload.stored_hash) {
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

    let created =
        verify_admit_import_sign_and_publish_staged_nar(&state, cache.id, &store, &key, &upload)
            .await?;
    let _ = store.discard(&key).await;

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
    use super::{
        MappedUpload, NarinfoPart, require_safe_hash, segment_names_stored_hash, stored_narinfo,
    };
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
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

        let stored = stored_narinfo(uploaded, &rotated()).unwrap();

        assert_eq!(stored.store_path, format!("/nix/store/{s}-hello"));
        assert_eq!(stored.references, vec![format!("/nix/store/{s}-hello")]);
        assert_eq!(stored.deriver, Some(format!("{s}-hello.drv")));
        assert_eq!(stored.file_size, 5);
    }

    #[test]
    fn an_uploaded_narinfo_with_a_short_hash_is_refused() {
        let o = EXAMPLE_ORIGINAL_HASH;
        assert!(stored_narinfo(part("abc-hello".into(), Vec::new()), &rotated()).is_err());
        assert!(
            stored_narinfo(
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

        assert!(segment_names_stored_hash(
            EXAMPLE_STORED_HASH,
            &upload.stored_hash
        ));
        assert!(segment_names_stored_hash(
            &format!("{EXAMPLE_STORED_HASH}-hello"),
            &upload.stored_hash
        ));
        assert!(!segment_names_stored_hash(
            EXAMPLE_ORIGINAL_HASH,
            &upload.stored_hash
        ));
    }

    #[test]
    fn accepts_a_normal_store_basename() {
        assert!(require_safe_hash("bnq5n76hrfr50l5s2hbbg9vw32fvcrbc-linux-rpi-6.12.75").is_ok());
    }

    #[test]
    fn rejects_traversal_and_separators() {
        for bad in ["", "../etc/passwd", "a/b", "..", "x/.."] {
            assert!(require_safe_hash(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
