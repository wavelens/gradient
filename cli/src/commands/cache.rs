/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::cache_nar;
use crate::commands::cache_sharing;
use crate::commands::cache_upload;
use crate::commands::cache_upstream;
use crate::commands::completion;
use crate::commands::webhook::{WebhookSubcommands, run_webhook_subcommand};
use crate::error::CliError;
use crate::input::{CommandLineFormField, ask_for_secret, client_from_config, handle_input};
use crate::output::{ExitKind, Output};
use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use connector::api::{MakeCacheRequest, PatchCacheRequest};
use connector::webhooks::WebhookResource;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Create a cache with missing fields opened in $EDITOR
    Create {
        /// Name
        #[arg(short, long)]
        name: Option<String>,
        /// Display name
        #[arg(short, long)]
        display_name: Option<String>,
        /// Description
        #[arg(short = 'c', long)]
        description: Option<String>,
        /// Priority advertised to Nix, lower is preferred
        #[arg(short, long)]
        priority: Option<i32>,
        /// Max cache storage in GB. 0 = unlimited (default), otherwise at least 1.
        #[arg(short = 'm', long, default_value_t = 0)]
        max_storage_gb: i64,
    },
    /// List the caches of the current user
    List,
    /// Edit a cache
    Edit {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
        /// New display name
        #[arg(short, long)]
        display_name: Option<String>,
        /// New description
        #[arg(short = 'c', long)]
        description: Option<String>,
        /// New priority, lower is preferred by Nix
        #[arg(short, long)]
        priority: Option<i32>,
        /// Max cache storage in GB. 0 = unlimited, otherwise at least 1.
        #[arg(short = 'm', long)]
        max_storage_gb: Option<i64>,
        /// Answer Nix clients with paths from the upstream caches
        #[arg(long)]
        pull_through: Option<bool>,
    },
    /// Delete a cache
    Delete {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// Print the public key of a cache
    Show {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// Build a netrc entry locally for a cache and install it into a netrc file.
    /// No request is sent to the server - bring your own API key (issued via the
    /// frontend or `gradient` UI). Intended to be run as root (e.g. sudo) to
    /// install into /etc/nix/netrc.
    InstallNetrc {
        /// Gradient server URL (e.g. https://gradient.example.com)
        #[arg(short, long)]
        server: String,
        /// Bearer token (`GRAD…` API key or JWT). Prompted on stdin if omitted.
        #[arg(short, long)]
        token: Option<String>,
        /// Name of the cache (used purely as a label in the netrc entry)
        #[arg(short, long, add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Path to the netrc file to update (default: /etc/nix/netrc)
        #[arg(short = 'f', long, default_value = "/etc/nix/netrc")]
        netrc_file: String,
    },
    /// Manage individual NARs in a cache
    Nar {
        #[command(subcommand)]
        cmd: cache_nar::Commands,
    },
    /// Upload NAR(s) to a cache
    Upload(crate::commands::cache_upload::UploadArgs),
    /// Re-enable a Disabled Cache
    Enable {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// Disable the cache. Nix clients and builds no longer use that cache until a later command can turn that cache back on.
    Disable {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// Allow anyone to read the cache without login
    Publish {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// Make a public cache private again.
    Unpublish {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// List projects that subscribe to caches
    Subscribers {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        name: String,
    },
    /// Manage the users that are members of a cache.
    User {
        #[command(subcommand)]
        cmd: cache_sharing::CacheUserCommands,
    },
    /// List or revoke pending membership invitations for a cache.
    Invitation {
        #[command(subcommand)]
        cmd: cache_sharing::CacheInvitationCommands,
    },
    /// Manage custom roles for the cache
    Role {
        #[command(subcommand)]
        cmd: cache_sharing::CacheRoleCommand,
    },
    /// Manage the teams that can access the cache.
    Team {
        #[command(subcommand)]
        cmd: cache_sharing::CacheTeamCommand,
    },
    /// Approve or deny requests from a project to subscribe to the cache.
    Request {
        #[command(subcommand)]
        cmd: cache_sharing::CacheRequestSubcommand,
    },
    /// Manage upstream caches that provide store paths to the cache.
    Upstream {
        #[command(subcommand)]
        cmd: cache_upstream::CacheUpstreamCommands,
    },
    /// Manage cache webhooks
    Webhook {
        #[command(subcommand)]
        cmd: WebhookSubcommands<CacheWebhookListArguments>,
    },
}

#[derive(Args, Debug)]
pub struct CacheWebhookListArguments {
    /// Cache Name
    #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
    cache: String,
}

pub async fn handle(cmd: Commands, out: Output) {
    match cmd {
        Commands::Create {
            name,
            display_name,
            description,
            priority,
            max_storage_gb,
        } => {
            let input_fields = vec![
                CommandLineFormField::new_non_empty("Name", name),
                CommandLineFormField::new_non_empty("Display Name", display_name),
                CommandLineFormField::new_optional_form_field("Description", description),
                CommandLineFormField::new_non_empty("Priority", priority.map(|p| p.to_string())),
                CommandLineFormField::new_non_empty("Max Storage (GB)", None)
                    .default_value(max_storage_gb.to_string()),
            ];

            let input = handle_input(input_fields, false, out);
            let name = input.get("Name");

            let priority = match input.get("Priority").parse::<i32>() {
                Ok(p) => p,
                Err(_) => out.fail(CliError::NotAnInteger("Priority")),
            };
            let max_storage_gb = parse_max_storage_gb(&input.get("Max Storage (GB)"), out);

            let client = client_from_config(out);
            match client
                .caches()
                .create(MakeCacheRequest {
                    name: name.into(),
                    display_name: input.get("Display Name").into(),
                    description: input.get("Description"),
                    priority,
                    max_storage_gb: Some(max_storage_gb),
                    local_priority: None,
                    public: None,
                    pull_through: None,
                })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"created": true}));
                    out.human("Cache created.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::List => {
            let client = client_from_config(out);
            match client.caches().list().await {
                Ok(res) => {
                    out.ok(&res);
                    if res.items.is_empty() {
                        out.human("You have no caches.");
                    } else {
                        for cache in res.items {
                            out.human(format!("{}: {}", cache.name, cache.id));
                        }
                    }
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Edit {
            name,
            display_name,
            description,
            priority,
            max_storage_gb,
            pull_through,
        } => {
            let client = client_from_config(out);
            let current = match client.caches().get(&name).await {
                Ok(c) => c,
                Err(e) => out.fail(e),
            };

            let input_fields = vec![
                CommandLineFormField::new_non_empty("Display Name", display_name)
                    .default_value(current.display_name),
                CommandLineFormField::new_optional_form_field("Description", description)
                    .default_value(current.description),
                CommandLineFormField::new_non_empty("Priority", priority.map(|p| p.to_string()))
                    .default_value(current.priority.to_string()),
                CommandLineFormField::new_non_empty(
                    "Max Storage (GB)",
                    max_storage_gb.map(|gb| gb.to_string()),
                )
                .default_value(current.max_storage_gb.to_string()),
            ];

            let input = handle_input(input_fields, pull_through.is_some(), out);

            let priority = match input.get("Priority").parse::<i32>() {
                Ok(p) => p,
                Err(_) => out.fail(CliError::NotAnInteger("Priority")),
            };

            match client
                .caches()
                .update(
                    &name,
                    PatchCacheRequest {
                        display_name: Some(input.get("Display Name")),
                        description: Some(input.get("Description")),
                        priority: Some(priority),
                        max_storage_gb: Some(parse_max_storage_gb(
                            &input.get("Max Storage (GB)"),
                            out,
                        )),
                        pull_through,
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Cache updated.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Delete { name } => {
            let client = client_from_config(out);
            match client.caches().delete(&name).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Cache deleted.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Show { name } => {
            let client = client_from_config(out);
            match client.caches().public_key(&name).await {
                Ok(key) => {
                    out.ok(&serde_json::json!({"public_key": key}));
                    out.human(format!("Cache Public Key: {}", key));
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::InstallNetrc {
            server,
            token,
            cache,
            netrc_file,
        } => {
            let token = match token {
                Some(t) if !t.is_empty() => t,
                _ if out.is_json() => out.fail(CliError::MissingArgument("--token")),
                _ => ask_for_secret("API key (GRAD…)", out),
            };

            if token.contains(char::is_whitespace) {
                out.err(
                    ExitKind::Usage,
                    "API keys cannot contain spaces or newlines.",
                );
            }

            let machine_host = crate::netrc::machine_host(&server);
            if machine_host.is_empty() {
                out.err(ExitKind::Usage, format!("Invalid server URL: '{}'", server));
            }

            crate::netrc::replace_entry_for_host_atomically(
                std::path::Path::new(&netrc_file),
                &machine_host,
                &token,
            )
            .unwrap_or_else(|e| out.fail(e));

            out.ok(&serde_json::json!({
                "installed": true,
                "cache": cache,
                "machine": machine_host,
                "netrc_file": netrc_file,
            }));
            out.human(format!(
                "netrc credentials for cache '{}' (machine '{}') installed into '{}'.",
                cache, machine_host, netrc_file
            ));
        }

        Commands::Nar { cmd } => cache_nar::handle(cmd, out).await,
        Commands::Upload(args) => cache_upload::handle(args, out).await,
        Commands::Enable { name } => {
            let client = client_from_config(out);
            match client.caches().activate_cache(&name).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"active": true}));
                    out.human("Cache enabled.");
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::Disable { name } => {
            let client = client_from_config(out);
            match client.caches().deactivate_cache(&name).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"active": false}));
                    out.human("Cache disabled.");
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::Publish { name } => {
            let client = client_from_config(out);
            match client.caches().make_cache_public(&name).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"public": true}));
                    out.human("The cache is now public.");
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::Unpublish { name } => {
            let client = client_from_config(out);
            match client.caches().make_cache_private(&name).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"public": false}));
                    out.human("The cache is private now.");
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::Subscribers { name } => {
            let client = client_from_config(out);
            match client
                .caches()
                .list_projects_subscribed_to_cache(&name)
                .await
            {
                Ok(subscribers) => {
                    out.ok(&subscribers);
                    if subscribers.projects.is_empty() && subscribers.hidden == 0 {
                        out.human("No projects subscribe to this cache.");
                    }
                    for project in &subscribers.projects {
                        out.human(format!(
                            "{} ({}): {}",
                            project.project, project.project_display_name, project.mode
                        ));
                    }
                    if subscribers.hidden > 0 {
                        out.human(format!(
                            "{} more projects remain hidden.",
                            subscribers.hidden
                        ));
                    }
                }
                Err(e) => out.fail(e),
            }
        }
        Commands::User { cmd } => cache_sharing::handle_cache_user_commands(cmd, out).await,
        Commands::Invitation { cmd } => {
            cache_sharing::apply_cache_invitation_commands(cmd, out).await
        }
        Commands::Role { cmd } => cache_sharing::apply_cache_role_command(cmd, out).await,
        Commands::Team { cmd } => cache_sharing::execute_cache_team_command(cmd, out).await,
        Commands::Request { cmd } => {
            cache_sharing::respond_to_subscription_cache_requests(cmd, out).await
        }
        Commands::Upstream { cmd } => cache_upstream::handle(cmd, out).await,
        Commands::Webhook { cmd } => {
            run_webhook_subcommand(
                cmd,
                |owner| WebhookResource::Cache(owner.cache.clone()),
                out,
            )
            .await
        }
    }
}

fn parse_max_storage_gb(raw: &str, out: Output) -> i64 {
    match raw.trim().parse::<i64>() {
        Ok(v) if v >= 0 => v,
        Ok(_) => out.err(
            ExitKind::Usage,
            "Max storage must be 0 (unlimited) or at least 1.",
        ),
        Err(_) => out.fail(CliError::NotAnInteger("Max storage")),
    }
}
