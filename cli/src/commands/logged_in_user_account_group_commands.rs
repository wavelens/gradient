/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::config::*;
use crate::error::{CliError, FileAction};
use crate::input::{ask_for_input, ask_for_password, client_from_config};
use crate::output::{ExitKind, Output};
use clap::Subcommand;
use connector::Client;
use connector::api::{
    ApiKeyInfo, CreateApiKeyRequest, CreateSshKeyRequest, DeleteUserRequest, PatchApiKeyRequest,
};
use std::num::NonZeroU32;
use std::path::PathBuf;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Manage SSH keys for login to the Nix daemon.
    SshPublicKeys {
        #[command(subcommand)]
        cmd: SshKeyCommands,
    },
    /// Management of API Keys
    ApiKeys {
        #[command(subcommand)]
        cmd: LoggedInUserApiKeyCommands,
    },
    /// Manage Login Sessions
    ActiveLoginSessions {
        #[command(subcommand)]
        cmd: LoggedInUserSessionCommands,
    },
    /// Manage projects, caches and team invitations
    Invitations {
        #[command(subcommand)]
        cmd: InvitationCommands,
    },
    /// Manage the accounts of login providers that are linked to this account.
    LinkedExternalLoginAccounts {
        #[command(subcommand)]
        cmd: LinkedLoginProviderIdentityCommands,
    },
    /// Print the settings of the account
    PrintAccountSettings,
    /// Delete the logged-in account and all account data completely. Then log out.
    DeleteAccount {
        /// Current password. Required together with `--json` for accounts that have a password.
        #[arg(long)]
        password: Option<String>,
        /// Enter the username again as confirmation. Accounts without a password require the username together with `--json`.
        #[arg(long = "confirm-username", conflicts_with = "password")]
        confirm_username: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum SshKeyCommands {
    /// List SSH Keys
    List,
    /// Add a Public Key for SSH
    Add {
        /// Key name
        name: String,
        /// OpenSSH public key (such as the contents of ~/.ssh/id_ed25519.pub)
        #[arg(required_unless_present = "file", conflicts_with = "file")]
        public_key: Option<String>,
        /// Read the public key from this file
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Delete SSH Key
    Delete {
        /// SSH key ID from `list`
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum LoggedInUserApiKeyCommands {
    /// Show the API Keys
    List,
    /// Create an API key. The key can only appear once. The API can't show the key again.
    Create {
        /// Name of the API key
        name: String,
        /// Permission to grant. Repeatable. `permissions` lists them all
        #[arg(long = "permission", required = true)]
        permissions: Vec<String>,
        /// Expire the key after this number of days
        #[arg(long)]
        expires_in_days: Option<NonZeroU32>,
        /// Limit the key to this project
        #[arg(long, conflicts_with = "cache")]
        api_key_project: Option<String>,
        /// Limit the key to this cache. The key has cache permissions only.
        #[arg(long)]
        cache: Option<String>,
        /// Accept keys only from this IP address or CIDR range. Use the flag more than once.
        #[arg(long = "allowed-ip")]
        allowed_ips: Vec<String>,
    },
    /// Edit the API key. The specified permissions or IP addresses replace the current ones.
    Edit {
        /// API key name or ID
        key: String,
        /// New Name
        #[arg(long)]
        name: Option<String>,
        /// Permission to grant. Repeatable. `permissions` lists them all
        #[arg(long = "permission")]
        permissions: Vec<String>,
        /// Limit the key to this project
        #[arg(long)]
        api_key_project: Option<String>,
        /// Accept keys only from this IP address or CIDR range. Use the flag more than once.
        #[arg(long = "allowed-ip")]
        allowed_ips: Vec<String>,
    },
    /// Revoke the API key. Revoked keys remain in the list for the audit trail.
    Revoke {
        /// API key name or ID
        key: String,
    },
    /// Delete API key
    Delete {
        /// API key name or ID
        key: String,
    },
    /// List the permissions that an API key can grant
    Permissions,
}

#[derive(Subcommand, Debug)]
pub enum LoggedInUserSessionCommands {
    /// List of Active Sessions
    List {
        /// Page number (first page is 1)
        #[arg(long)]
        page: Option<u64>,
        /// Sessions per page (max 100)
        #[arg(long = "per-page")]
        per_page: Option<u64>,
    },
    /// Revoke the session and sign out the device.
    Revoke {
        /// Session ID from `list`
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum InvitationCommands {
    /// List pending invitations
    List,
    /// Accept an invitation
    Accept {
        /// Invitation token from `list`
        token: String,
    },
    /// Decline Invitation
    Decline {
        /// Invitation token from `list`
        token: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum LinkedLoginProviderIdentityCommands {
    /// Show a list of login providers and the link state of each one.
    List,
    /// Unlink the account of a login provider
    Disconnect {
        /// ID of the login provider from `list`
        provider: String,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    let client = client_from_config(out);
    let user = client.user();

    match cmd {
        Commands::SshPublicKeys { cmd } => match cmd {
            SshKeyCommands::List => match user.get_ssh_keys().await {
                Ok(keys) => {
                    out.ok(&keys);
                    if keys.is_empty() {
                        out.human("No SSH keys.");
                    }
                    for key in keys {
                        out.human(format!("{}: {} {}", key.id, key.name, key.fingerprint));
                    }
                }
                Err(e) => out.fail(e),
            },
            SshKeyCommands::Add {
                name,
                public_key,
                file,
            } => {
                let public_key = match (public_key, file) {
                    (Some(key), _) => key,
                    (None, Some(path)) => std::fs::read_to_string(&path)
                        .unwrap_or_else(|source| {
                            out.fail(CliError::File {
                                action: FileAction::Read,
                                path,
                                reason: source,
                            })
                        })
                        .trim()
                        .to_owned(),
                    (None, None) => out.fail(CliError::MissingArgument("PUBLIC_KEY")),
                };
                match user
                    .add_user_ssh_key(CreateSshKeyRequest { name, public_key })
                    .await
                {
                    Ok(key) => {
                        out.ok(&key);
                        out.human("Added an SSH key.");
                    }
                    Err(e) => out.fail(e),
                }
            }
            SshKeyCommands::Delete { id } => match user.delete_user_ssh_key(&id).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true, "id": id}));
                    out.human("SSH key deleted.");
                }
                Err(e) => out.fail(e),
            },
        },

        Commands::ApiKeys { cmd } => {
            match cmd {
                LoggedInUserApiKeyCommands::List => match user.keys().await {
                    Ok(keys) => {
                        out.ok(&keys);
                        if keys.is_empty() {
                            out.human("No API key found.");
                        }
                        for key in keys {
                            let revoked = if key.revoked_at.is_some() {
                                " (revoked)"
                            } else {
                                ""
                            };
                            out.human(format!("{}: {}{revoked}", key.id, key.name));
                        }
                    }
                    Err(e) => out.fail(e),
                },
                LoggedInUserApiKeyCommands::Create {
                    name,
                    permissions,
                    expires_in_days,
                    api_key_project,
                    cache,
                    allowed_ips,
                } => {
                    match user
                        .create_key(CreateApiKeyRequest {
                            name,
                            permissions,
                            expires_in_days,
                            project: api_key_project,
                            cache,
                            allowed_ips: (!allowed_ips.is_empty()).then_some(allowed_ips),
                        })
                        .await
                    {
                        Ok(token) => {
                            out.ok(&serde_json::json!({"token": token}));
                            out.human(
                                "Copy the API key now. This key is not available after this:",
                            );
                            out.human(token);
                        }
                        Err(e) => out.fail(e),
                    }
                }
                LoggedInUserApiKeyCommands::Edit {
                    key,
                    name,
                    permissions,
                    api_key_project,
                    allowed_ips,
                } => {
                    let key = find_user_api_key_by_id_or_name(&client, &key, out).await;
                    match user
                        .edit_api_key(
                            &key.id,
                            PatchApiKeyRequest {
                                name,
                                permissions: (!permissions.is_empty()).then_some(permissions),
                                project: api_key_project,
                                allowed_ips: (!allowed_ips.is_empty()).then_some(allowed_ips),
                            },
                        )
                        .await
                    {
                        Ok(updated) => {
                            out.ok(&updated);
                            out.human("API key update succeeded.");
                        }
                        Err(e) => out.fail(e),
                    }
                }
                LoggedInUserApiKeyCommands::Revoke { key } => {
                    let key = find_user_api_key_by_id_or_name(&client, &key, out).await;
                    match user.revoke_key(&key.id).await {
                        Ok(message) => {
                            out.ok(&serde_json::json!({"revoked": true, "id": key.id, "message": message}));
                            out.human(format!("{message}."));
                        }
                        Err(e) => out.fail(e),
                    }
                }
                LoggedInUserApiKeyCommands::Delete { key } => {
                    let key = find_user_api_key_by_id_or_name(&client, &key, out).await;
                    match user.delete_api_key_by_name(&key.name).await {
                        Ok(message) => {
                            out.ok(&serde_json::json!({"deleted": true, "id": key.id, "message": message}));
                            out.human(format!("{message}."));
                        }
                        Err(e) => out.fail(e),
                    }
                }
                LoggedInUserApiKeyCommands::Permissions => match user.key_permissions().await {
                    Ok(catalogue) => {
                        out.ok(&catalogue);
                        for permission in &catalogue.available_permissions {
                            out.human(format!(
                                "{}: {}",
                                permission.id, permission.permission_explanation
                            ));
                        }
                    }
                    Err(e) => out.fail(e),
                },
            }
        }

        Commands::ActiveLoginSessions { cmd } => match cmd {
            LoggedInUserSessionCommands::List { page, per_page } => {
                match user.list_sessions(page, per_page).await {
                    Ok(sessions) => {
                        out.ok(&sessions);
                        for session in &sessions.items {
                            let current = if session.current { " (current)" } else { "" };
                            out.human(format!(
                                "{}: {} {}{current}",
                                session.id,
                                session.last_used_at,
                                session.user_agent.as_deref().unwrap_or("-"),
                            ));
                        }
                        out.human(format!(
                            "page {} ({} per page, total {})",
                            sessions.page, sessions.per_page, sessions.total
                        ));
                    }
                    Err(e) => out.fail(e),
                }
            }
            LoggedInUserSessionCommands::Revoke { id } => match user.delete_session(&id).await {
                Ok(message) => {
                    out.ok(&serde_json::json!({"revoked": true, "id": id}));
                    out.human(format!("{message}."));
                }
                Err(e) => out.fail(e),
            },
        },

        Commands::Invitations { cmd } => {
            let result = match cmd {
                InvitationCommands::List => {
                    match user.list_invitations_addressed_to_me().await {
                        Ok(invites) => {
                            out.ok(&invites);
                            if invites.is_empty() {
                                out.human("No pending invitations.");
                            }
                            for invite in invites {
                                out.human(format!(
                                    "{} {} as {} from {}: {}",
                                    invite.kind,
                                    invite.scope,
                                    invite.role,
                                    invite.invited_by,
                                    invite.token
                                ));
                            }
                        }
                        Err(e) => out.fail(e),
                    }
                    return;
                }
                InvitationCommands::Accept { token } => user.accept_user_invite(&token).await,
                InvitationCommands::Decline { token } => user.decline_user_invite(&token).await,
            };
            match result {
                Ok(message) => {
                    out.ok(&serde_json::json!({"message": message}));
                    out.human(format!("{message}."));
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::LinkedExternalLoginAccounts { cmd } => match cmd {
            LinkedLoginProviderIdentityCommands::List => match user.get_user_settings().await {
                Ok(settings) => {
                    out.ok(&settings.login_providers);
                    for provider in settings.login_providers {
                        out.human(format!(
                            "{} ({}): connected={}",
                            provider.id, provider.label, provider.connected
                        ));
                    }
                }
                Err(e) => out.fail(e),
            },
            LinkedLoginProviderIdentityCommands::Disconnect { provider } => {
                match user.disconnect_user_linked_login_provider(&provider).await {
                    Ok(message) => {
                        out.ok(&serde_json::json!({"disconnected": true, "provider": provider}));
                        out.human(format!("{message}."));
                    }
                    Err(e) => out.fail(e),
                }
            }
        },

        Commands::PrintAccountSettings => match user.get_user_settings().await {
            Ok(settings) => {
                out.ok(&settings);
                out.human(format!("Username: {}", settings.username));
                out.human(format!("Name: {}", settings.name));
                out.human(format!("Email: {}", settings.email));
                out.human(format!("Password: {}", settings.has_password));
                out.human(format!("Managed: {}", settings.managed));
            }
            Err(e) => out.fail(e),
        },

        Commands::DeleteAccount {
            password,
            confirm_username,
        } => {
            let body = match (password, confirm_username) {
                (Some(password), _) => DeleteUserRequest {
                    password: Some(password),
                    confirm_username: None,
                },
                (None, Some(username)) => DeleteUserRequest {
                    password: None,
                    confirm_username: Some(username),
                },
                (None, None) if out.is_json() => out.fail(CliError::MissingArgument("--password")),
                (None, None) => match user.get_user_settings().await {
                    Ok(settings) if settings.has_password => DeleteUserRequest {
                        password: Some(ask_for_password(out)),
                        confirm_username: None,
                    },
                    Ok(_) => DeleteUserRequest {
                        password: None,
                        confirm_username: Some(ask_for_input("Enter a username to confirm", out)),
                    },
                    Err(e) => out.fail(e),
                },
            };
            match user.delete_own_account(body).await {
                Ok(_) => {
                    set_value(ConfigKey::AuthToken, String::new(), true);
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Account deleted. Logged out.");
                }
                Err(e) => out.fail(e),
            }
        }
    }
}

async fn find_user_api_key_by_id_or_name(client: &Client, key: &str, out: Output) -> ApiKeyInfo {
    let keys = client.user().keys().await.unwrap_or_else(|e| out.fail(e));
    keys.iter()
        .find(|candidate| candidate.id == key)
        .or_else(|| keys.iter().find(|candidate| candidate.name == key))
        .cloned()
        .unwrap_or_else(|| {
            out.err(
                ExitKind::Usage,
                format!("No API keys have name or ID '{key}'."),
            )
        })
}
