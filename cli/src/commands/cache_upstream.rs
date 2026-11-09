/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::error::CliError;
use crate::input::client_from_config;
use crate::output::{ExitKind, Output};
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::api::{AddUpstreamRequest, CacheSubscriptionMode, PatchUpstreamRequest};

#[derive(Subcommand, Debug)]
pub enum CacheUpstreamCommands {
    /// List upstream caches of a cache
    List {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Add an upstream cache. Use `--internal-cache-name` for caches on this server. Use `--url` and `--public-key` for Nix binary caches. Use `--url` and `--remote-cache-name` for caches on another Gradient server.
    Add {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Display Name of the Upstream Cache
        #[arg(long)]
        display_name: Option<String>,
        /// Name of the cache on this server
        #[arg(long, conflicts_with_all = ["url", "public_key", "remote_cache_name", "http1_only", "api_key"])]
        internal_cache_name: Option<String>,
        /// URL of the upstream cache
        #[arg(long)]
        url: Option<String>,
        /// Public signing key for a Nix binary cache (example: `cache.nixos.org-1:...`)
        #[arg(long, requires = "url", conflicts_with_all = ["remote_cache_name", "api_key"])]
        public_key: Option<String>,
        /// Communicate with the upstream cache over HTTP/1.1 only.
        #[arg(long, requires = "public_key")]
        http1_only: bool,
        /// Name of the cache on a different Gradient server
        #[arg(long, requires = "url")]
        remote_cache_name: Option<String>,
        /// API key for another Gradient server
        #[arg(long, requires = "remote_cache_name")]
        api_key: Option<String>,
        /// Access mode: ReadWrite, ReadOnly, or WriteOnly
        #[arg(long)]
        cache_access_mode: Option<CacheSubscriptionMode>,
    },
    /// Edit the upstream cache
    Edit {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Upstream cache ID
        id: String,
        /// Display Name of the Upstream Cache
        #[arg(long)]
        display_name: Option<String>,
        /// Access mode: ReadWrite, ReadOnly, or WriteOnly
        #[arg(long)]
        cache_access_mode: Option<CacheSubscriptionMode>,
        /// URL of the upstream cache
        #[arg(long)]
        url: Option<String>,
        /// Public signing key for a Nix binary cache (example: `cache.nixos.org-1:...`)
        #[arg(long)]
        public_key: Option<String>,
        /// Communicate with the upstream cache over HTTP/1.1 only.
        #[arg(long)]
        http1_only: Option<bool>,
        /// Turn the upstream cache on or off.
        #[arg(long)]
        active: Option<bool>,
    },
    /// Delete the upstream cache.
    Delete {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Upstream cache ID
        id: String,
    },
    /// Probe the HTTP upstream cache via HTTP/1.1 and HTTP/2
    Test {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Upstream cache ID
        id: String,
    },
}

pub async fn handle(cmd: CacheUpstreamCommands, out: Output) {
    let client = client_from_config(out);
    let caches = client.caches();
    match cmd {
        CacheUpstreamCommands::List { cache } => match caches.upstream_caches(&cache).await {
            Ok(upstreams) => {
                out.ok(&upstreams);
                if upstreams.is_empty() {
                    out.human("The cache has no upstream cache.");
                }
                for upstream in &upstreams {
                    let target = upstream
                        .upstream_cache_name
                        .as_deref()
                        .or(upstream.url.as_deref())
                        .unwrap_or("-");
                    let inactive = if upstream.active { "" } else { " [Inactive]" };
                    out.human(format!(
                        "{} {} ({}, {}): {}{}",
                        upstream.id.0,
                        upstream.display_name,
                        upstream.kind,
                        upstream.mode,
                        target,
                        inactive
                    ));
                }
            }
            Err(e) => out.fail(e),
        },
        CacheUpstreamCommands::Add {
            cache,
            display_name,
            internal_cache_name,
            url,
            public_key,
            http1_only,
            remote_cache_name,
            api_key,
            cache_access_mode,
        } => {
            let required = |value: Option<String>, flag: &'static str| {
                value.unwrap_or_else(|| out.fail(CliError::MissingArgument(flag)))
            };
            let body = match (internal_cache_name, public_key, remote_cache_name) {
                (Some(cache_name), None, None) => AddUpstreamRequest::Internal {
                    cache_name,
                    display_name,
                    mode: cache_access_mode,
                },
                (None, Some(public_key), None) => AddUpstreamRequest::Http {
                    display_name: required(display_name, "--display-name"),
                    http1_only: Some(http1_only),
                    public_key,
                    url: required(url, "--url"),
                },
                (None, None, Some(remote_cache)) => AddUpstreamRequest::GradientProto {
                    api_key,
                    display_name: required(display_name, "--display-name"),
                    mode: cache_access_mode,
                    remote_cache,
                    url: required(url, "--url"),
                },
                _ => out.err(
                    ExitKind::Usage,
                    "Specify exactly one of --internal-cache-name, --public-key, or --remote-cache-name.",
                ),
            };
            match caches.add_upstream_cache(&cache, body).await {
                Ok(id) => {
                    out.ok(&serde_json::json!({"id": id}));
                    out.human(format!("Added upstream cache: {}", id.0));
                }
                Err(e) => out.fail(e),
            }
        }
        CacheUpstreamCommands::Edit {
            cache,
            id,
            display_name,
            cache_access_mode,
            url,
            public_key,
            http1_only,
            active,
        } => {
            let body = PatchUpstreamRequest {
                display_name,
                mode: cache_access_mode,
                url,
                public_key,
                http1_only,
                active,
            };
            match caches.update_upstream_cache(&cache, &id, body).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Upstream cache updated.");
                }
                Err(e) => out.fail(e),
            }
        }
        CacheUpstreamCommands::Delete { cache, id } => {
            match caches.delete_upstream_cache(&cache, &id).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Upstream cache deleted.");
                }
                Err(e) => out.fail(e),
            }
        }
        CacheUpstreamCommands::Test { cache, id } => {
            match caches.test_upstream_cache(&cache, &id).await {
                Ok(result) => {
                    out.ok(&result);
                    out.human(&result.message);
                    for (protocol, probe) in
                        [("HTTP/1.1", &result.http1), ("HTTP/2", &result.http2)]
                    {
                        let outcome = match (&probe.error, probe.status) {
                            (Some(error), _) => error.clone(),
                            (None, Some(status)) => status.to_string(),
                            (None, None) => "-".to_owned(),
                        };
                        out.human(format!("{protocol}: {outcome} ({} ms)", probe.latency_ms));
                    }
                }
                Err(e) => out.fail(e),
            }
        }
    }
}
