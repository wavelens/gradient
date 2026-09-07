/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::helpers::{CacheContext, JsonFlag, cache_client_ip, get_nar_by_hash};
use crate::client_ip::OptionalPeer;
use crate::error::{WebError, WebResult};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use gradient_core::ServerState;
use gradient_core::upstream::UpstreamProbe;
use gradient_sources::{CacheSigner, get_hash_from_url};
use gradient_types::*;
use gradient_util::http;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use tracing::warn;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn text_response(content_type: &'static str, body: String) -> WebResult<Response<String>> {
    Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .body(body)
        .map_err(|e| WebError::internal(format!("Failed to build response: {}", e)))
}

/// Attach cache-observability headers to a narinfo response: `X-Cache` reports
/// whether it was served from our store (`HIT`) or proxied from an upstream
/// (`MISS`), and CORS is opened for browser-based Nix tooling.
fn with_narinfo_headers(mut response: Response, cache_status: &'static str) -> Response {
    let headers = response.headers_mut();
    headers.insert("x-cache", HeaderValue::from_static(cache_status));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

// ── Handlers ──────────────────────────────────────────────────────────────────

pub async fn nix_cache_info(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path(cache): Path<String>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state, &headers, peer);
    let ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;

    let priority = match (ctx.cache.local_priority, peer) {
        (Some(p), Some(addr)) if p != 0 => {
            let client_ip = crate::client_ip::resolve_client_ip(
                &headers,
                addr.ip(),
                &state.config.network.trusted_proxies,
            );
            if in_any(client_ip, &state.config.network.local_ips) {
                p
            } else {
                ctx.cache.priority
            }
        }
        _ => ctx.cache.priority,
    };

    let res = NixCacheInfo {
        want_mass_query: true,
        store_dir: "/nix/store".to_string(),
        priority,
    };

    if flag.is_set() {
        Ok(axum::Json(res).into_response())
    } else {
        Ok(text_response("text/x-nix-cache-info", res.to_nix_string())?.into_response())
    }
}

pub async fn gradient_cache_info(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path(cache): Path<String>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    let client_ip = cache_client_ip(&state, &headers, peer);
    CacheContext::load(&state, &headers, client_ip, cache).await?;

    let info = GradientCacheInfo {
        gradient_version: env!("CARGO_PKG_VERSION").to_string(),
        gradient_url: state.config.server.serve_url.clone(),
    };

    let mut response = if flag.is_set() {
        axum::Json(info).into_response()
    } else {
        text_response("text/x-gradient-cache-info", info.to_nix_string())?.into_response()
    };
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    Ok(response)
}

pub async fn path(
    state: State<Arc<ServerState>>,
    OptionalPeer(peer): OptionalPeer,
    headers: HeaderMap,
    Path((cache, path)): Path<(String, String)>,
    Query(flag): Query<JsonFlag>,
) -> WebResult<Response> {
    // Anything that isn't a narinfo is simply not in this cache. Nix clients and
    // debuginfod probe the cache root for keys we never serve, and a 4xx other
    // than 404 reads as a hard error to them - nixseparatedebuginfod aborts the
    // whole request rather than moving on to the next substituter (#563).
    if !path.ends_with(".narinfo") {
        return Err(WebError::not_found("Path"));
    }

    let path_hash = get_hash_from_url(path.clone()).map_err(|_| WebError::not_found("Path"))?;

    let client_ip = cache_client_ip(&state, &headers, peer);
    let ctx = CacheContext::load(&state, &headers, client_ip, cache).await?;

    if let Ok(path_info) =
        get_nar_by_hash(Arc::clone(&state), ctx.cache.clone(), path_hash.clone()).await
    {
        let response = if flag.is_set() {
            axum::Json(path_info).into_response()
        } else {
            text_response("text/x-nix-narinfo", path_info.to_nix_string())?.into_response()
        };
        return Ok(with_narinfo_headers(response, "HIT"));
    }

    let rewritten = fetch_from_upstream(&state, &ctx.cache, &path_hash).await;
    if let Some(body) = rewritten {
        let response = if flag.is_set() {
            match gradient_types::parse_narinfo_body(&body) {
                Ok(parsed) => axum::Json(parsed).into_response(),
                Err(_) => return Err(WebError::internal("Upstream narinfo malformed")),
            }
        } else {
            text_response("text/x-nix-narinfo", body)?.into_response()
        };
        return Ok(with_narinfo_headers(response, "MISS"));
    }

    Err(WebError::not_found("Path"))
}

/// One `Key: value` line of a narinfo. Matches the whole key, so `StorePath`
/// never picks up a longer field that starts with it.
fn narinfo_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    body.lines().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim)
    })
}

/// Add this cache's own signature to a narinfo we are proxying.
///
/// Everything we serve is then verifiable with the Gradient cache's key alone,
/// so a client needs to trust one key rather than the key of every cache we
/// happen to proxy. The upstream's own `Sig` lines are kept: they stay valid
/// (a signature covers the fingerprint, not the rewritten `URL:`), and anyone
/// who does trust that upstream can still check it independently.
///
/// Only ever called on a body whose upstream signature already verified -
/// signing an unverified narinfo would launder whatever an upstream said under
/// our own name. Returns `None` if the fingerprint fields cannot be read, so a
/// body we do not fully understand is passed through untouched rather than
/// signed over guessed values.
fn resign_narinfo(
    body: &str,
    sign: impl FnOnce(&str, &str, u64, &[String]) -> String,
) -> Option<String> {
    let store_path = narinfo_field(body, "StorePath")?;
    let nar_hash = narinfo_field(body, "NarHash")?;
    let nar_size: u64 = narinfo_field(body, "NarSize")?.parse().ok()?;
    let references: Vec<String> = narinfo_field(body, "References")
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    let token = sign(store_path, nar_hash, nar_size, &references);

    let mut out = String::with_capacity(body.len() + token.len() + 8);
    for line in body.lines() {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("Sig: ");
    out.push_str(&token);
    out.push('\n');
    Some(out)
}

/// Point the narinfo's `URL:` at our own proxy endpoint, so a client fetching
/// the NAR comes back through us rather than straight to the upstream.
fn rewrite_nar_url(body: &str, upstream: CacheUpstreamId) -> String {
    body.lines()
        .map(|line| {
            if let Some(nar_path) = line.strip_prefix("URL: ") {
                format!("URL: nar/upstream/{}/{}", upstream, nar_path.trim())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// The narinfo for a path we do not hold, from whichever upstream has it.
///
/// Probing is concurrent and bounded, and an upstream that stops answering is
/// taken out of rotation: a cache must answer a miss quickly, and serialising
/// this meant one unreachable upstream cost every miss the full client timeout -
/// long enough that a substituter's own stall detector fired first.
async fn fetch_from_upstream(
    state: &Arc<ServerState>,
    cache: &MCache,
    path_hash: &str,
) -> Option<String> {
    let upstreams: Vec<UpstreamProbe> = ECacheUpstream::find()
        .filter(CCacheUpstream::Cache.eq(cache.id))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|upstream| {
            let url = upstream.url?;
            let Some(public_key) = upstream.public_key else {
                warn!(upstream = %upstream.id, "upstream missing public_key; skipping");
                return None;
            };
            Some(UpstreamProbe {
                id: upstream.id,
                url,
                public_key,
            })
        })
        .collect();

    let found =
        gradient_core::upstream::fetch_narinfo_body(http::download_client(), &upstreams, path_hash)
            .await?;

    let body = rewrite_nar_url(&found.body, found.upstream);

    // Built only once we actually have something to serve, so a miss never pays
    // for reading and decrypting the cache's key.
    let signer = match CacheSigner::from_cache(
        &state.config.secrets.crypt_secret_file,
        cache,
        &state.config.server.serve_url,
    ) {
        Ok(signer) => signer,
        Err(e) => {
            warn!(cache = %cache.name, error = %e, "cannot re-sign a proxied narinfo; serving it with the upstream's signature only");
            return Some(body);
        }
    };

    let signed = resign_narinfo(&body, |store_path, nar_hash, nar_size, references| {
        signer.sign_narinfo(store_path, nar_hash, nar_size, references)
    });
    Some(signed.unwrap_or(body))
}

#[cfg(test)]
mod tests {
    use super::{narinfo_field, resign_narinfo, rewrite_nar_url};
    use gradient_types::ids::CacheUpstreamId;

    const UPSTREAM: &str = "StorePath: /nix/store/k3l5ywcvzijjjx44b9xrjbhr116sal9i-plotly-6.8.0\n\
URL: nar/0z7np.nar.zst\n\
Compression: zstd\n\
NarHash: sha256:1lld5kb21xphcav6fd553as8fr4yqiz6mdfhac5mjcqpqzdrx8z8\n\
NarSize: 85514096\n\
References: sdd13vm3yf8fwhhasc5r0fm2pkzq9cmx-narwhals-2.23.0 9ipfvwnqp1q8ijnmi5sxvlx9r8w34lw3-bash-5.3p15\n\
Sig: cache.nixos.org-1:AAAA\n";

    /// The point of re-signing: a client that trusts only this cache's key can
    /// use a path we proxied, without being handed every upstream's key too.
    #[test]
    fn a_proxied_narinfo_gains_our_own_signature() {
        let out = resign_narinfo(UPSTREAM, |_, _, _, _| "gradient.test-main:OURS".into())
            .expect("re-signed");

        assert!(out.contains("Sig: gradient.test-main:OURS"), "{out}");
        assert!(
            out.contains("Sig: cache.nixos.org-1:AAAA"),
            "the upstream's own signature stays, so anyone trusting it can still verify: {out}"
        );
        assert!(out.ends_with('\n'));
    }

    /// The signature covers the fingerprint, so these four inputs are the whole
    /// correctness of it: a wrong field silently produces a narinfo nix rejects.
    #[test]
    fn the_signature_covers_the_paths_own_fingerprint_fields() {
        let seen = std::cell::RefCell::new(None);
        resign_narinfo(UPSTREAM, |sp, nh, ns, refs| {
            *seen.borrow_mut() = Some((sp.to_owned(), nh.to_owned(), ns, refs.to_vec()));
            "k:v".into()
        })
        .expect("re-signed");

        let (store_path, nar_hash, nar_size, refs) = seen.into_inner().expect("signer was called");
        assert_eq!(
            store_path,
            "/nix/store/k3l5ywcvzijjjx44b9xrjbhr116sal9i-plotly-6.8.0"
        );
        assert_eq!(
            nar_hash,
            "sha256:1lld5kb21xphcav6fd553as8fr4yqiz6mdfhac5mjcqpqzdrx8z8"
        );
        assert_eq!(nar_size, 85514096);
        assert_eq!(refs.len(), 2, "{refs:?}");
        assert!(
            refs.iter().all(|r| !r.starts_with("/nix/store/")),
            "references stay bare; the fingerprint adds the prefix and sorts: {refs:?}"
        );
    }

    /// A body we cannot read the fingerprint out of must be passed through
    /// unsigned rather than signed over guessed values.
    #[test]
    fn an_unparseable_narinfo_is_not_signed() {
        assert!(resign_narinfo("Compression: zstd\n", |_, _, _, _| "k:v".into()).is_none());
    }

    #[test]
    fn a_field_lookup_does_not_match_a_longer_key() {
        assert_eq!(narinfo_field(UPSTREAM, "NarSize"), Some("85514096"));
        assert_eq!(narinfo_field("StorePathX: y\n", "StorePath"), None);
        assert_eq!(narinfo_field(UPSTREAM, "CA"), None);
    }

    /// A client must never be handed the upstream's own NAR URL: it would fetch
    /// straight from there, bypassing this cache entirely.
    #[test]
    fn the_nar_url_is_rewritten_through_our_proxy() {
        let id = CacheUpstreamId::new(uuid::Uuid::from_u128(7));
        let body = "StorePath: /nix/store/aaa-foo\nURL: nar/1abc.nar.xz\nNarSize: 12\n";

        let out = rewrite_nar_url(body, id);

        assert!(
            out.contains(&format!("URL: nar/upstream/{id}/nar/1abc.nar.xz")),
            "{out}"
        );
        assert!(out.contains("StorePath: /nix/store/aaa-foo"));
        assert!(out.ends_with('\n'));
    }
}
