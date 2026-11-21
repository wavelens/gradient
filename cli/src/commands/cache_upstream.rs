/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::commands::mcp_command_run_decisions::{
    McpToolCommandPermission::McpToolMayRunCommand,
    UNAVAILABLE_BECAUSE_COMMAND_REVEALS_OR_SETS_SECRET_VALUES,
};
use crate::error::CliError;
use crate::input::client_from_config;
use crate::output::{ExitKind, Output};
use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use connector::api::{
    AddUpstreamRequest, CacheSubscriptionMode, HttpUpstreamCredentialRequest,
    PatchHttpUpstreamCredentialRequest, PatchUpstreamRequest,
};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct UpstreamCacheCredentialArgs {
    /// Username for Basic authentication on the HTTP upstream cache
    #[arg(
        long,
        requires = "basic_auth_password_file",
        conflicts_with = "bearer_token_file"
    )]
    basic_auth_user: Option<String>,
    /// File with the password for basic auth. The command removes the trailing newline.
    #[arg(long, requires = "basic_auth_user")]
    basic_auth_password_file: Option<PathBuf>,
    /// File with the bearer token for the HTTP upstream cache. Trailing newline removed.
    #[arg(long)]
    bearer_token_file: Option<PathBuf>,
}

impl UpstreamCacheCredentialArgs {
    fn read_http_upstream_credential_request(
        self,
    ) -> Result<Option<HttpUpstreamCredentialRequest>, CliError> {
        match (
            self.basic_auth_user,
            self.basic_auth_password_file,
            self.bearer_token_file,
        ) {
            (Some(user), Some(password_file), None) => {
                Ok(Some(HttpUpstreamCredentialRequest::Basic {
                    user,
                    password: read_secret_from_file(&password_file)?,
                }))
            }
            (None, None, Some(token_file)) => Ok(Some(HttpUpstreamCredentialRequest::Bearer {
                token: read_secret_from_file(&token_file)?,
            })),
            _ => Ok(None),
        }
    }
}

fn read_secret_from_file(path: &Path) -> Result<String, CliError> {
    let content = fs::read_to_string(path)
        .map_err(|error| CliError::ReadInput(path.display().to_string(), error))?;
    let secret = content.trim_end_matches(['\n', '\r']);
    if secret.is_empty() {
        return Err(CliError::EmptyField(format!(
            "Secret file {path}",
            path = path.display()
        )));
    }
    Ok(secret.to_owned())
}

fn build_patch_http_upstream_credential_request(
    credential_args: UpstreamCacheCredentialArgs,
    remove_credential: bool,
) -> Result<Option<PatchHttpUpstreamCredentialRequest>, CliError> {
    if remove_credential {
        return Ok(Some(PatchHttpUpstreamCredentialRequest::None));
    }
    Ok(credential_args
        .read_http_upstream_credential_request()?
        .map(|credential| match credential {
            HttpUpstreamCredentialRequest::Basic { user, password } => {
                PatchHttpUpstreamCredentialRequest::Basic { user, password }
            }
            HttpUpstreamCredentialRequest::Bearer { token } => {
                PatchHttpUpstreamCredentialRequest::Bearer { token }
            }
        }))
}

#[derive(Subcommand, Debug)]
pub enum CacheUpstreamCommands {
    /// List upstream caches of a cache
    #[command(add = McpToolMayRunCommand)]
    List {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Add an upstream cache. Use `--internal-cache-name` for caches on this server. Use `--url` and `--public-key` for Nix binary caches. Use `--url` and `--remote-cache-name` for caches on another Gradient server.
    #[command(add = UNAVAILABLE_BECAUSE_COMMAND_REVEALS_OR_SETS_SECRET_VALUES)]
    Add {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Display Name of the Upstream Cache
        #[arg(long)]
        display_name: Option<String>,
        /// Name of the cache on this server
        #[arg(long, conflicts_with_all = ["url", "public_key", "remote_cache_name", "http1_only", "api_key", "basic_auth_user", "basic_auth_password_file", "bearer_token_file"])]
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
        #[arg(long, requires = "url", conflicts_with_all = ["basic_auth_user", "basic_auth_password_file", "bearer_token_file"])]
        remote_cache_name: Option<String>,
        /// API key for another Gradient server
        #[arg(long, requires = "remote_cache_name")]
        api_key: Option<String>,
        /// Access mode: ReadWrite, ReadOnly, or WriteOnly
        #[arg(long)]
        cache_access_mode: Option<CacheSubscriptionMode>,
        #[command(flatten)]
        credential_args: UpstreamCacheCredentialArgs,
    },
    /// Edit the upstream cache
    #[command(add = UNAVAILABLE_BECAUSE_COMMAND_REVEALS_OR_SETS_SECRET_VALUES)]
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
        #[command(flatten)]
        credential_args: UpstreamCacheCredentialArgs,
        /// Delete credentials stored in the HTTP upstream cache.
        #[arg(long, conflicts_with_all = ["basic_auth_user", "basic_auth_password_file", "bearer_token_file"])]
        remove_credential: bool,
    },
    /// Delete the upstream cache.
    #[command(add = McpToolMayRunCommand)]
    Delete {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Upstream cache ID
        id: String,
    },
    /// Probe the HTTP upstream cache via HTTP/1.1 and HTTP/2
    #[command(add = McpToolMayRunCommand)]
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
                    let scheme = upstream
                        .credential_scheme
                        .as_ref()
                        .map_or_else(|| "none".to_owned(), ToString::to_string);
                    let stored_url_marker = if upstream.stored_url_has_user_password_or_query {
                        " [URL with a username, password or query]"
                    } else {
                        ""
                    };
                    out.human(format!(
                        "{} {} ({}, {}, credential: {scheme}): {}{}{}",
                        upstream.id.0,
                        upstream.display_name,
                        upstream.kind,
                        upstream.mode,
                        target,
                        inactive,
                        stored_url_marker
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
            credential_args,
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
                    credential: credential_args
                        .read_http_upstream_credential_request()
                        .unwrap_or_else(|error| out.fail(error)),
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
            credential_args,
            remove_credential,
        } => {
            let body = PatchUpstreamRequest {
                credential: build_patch_http_upstream_credential_request(
                    credential_args,
                    remove_credential,
                )
                .unwrap_or_else(|error| out.fail(error)),
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
