/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::completion;
use crate::input::client_from_config;
use crate::output::Output;
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use connector::api::{
    AddMemberRequest, ChangeMemberRoleRequest, CreateRoleRequest, GrantCacheRequest, IndexName,
    PatchCacheGrantRequest, PatchRoleRequest, RemoveMemberRequest, RevokeInvitationRequest,
    RoleResponseCacheRoleOwner,
};

#[derive(Subcommand, Debug)]
pub enum CacheUserCommands {
    /// Show cache members
    List {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Invite a user to the cache with a role.
    Add {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Username of the user
        user: String,
        /// Role name, such as `Admin`, `Write`, `View`, or a custom role.
        role: String,
        /// Add the user immediately without sending an invite. Valid for instance admins, members of the owner team and users who already have access from a team grant.
        #[arg(long)]
        add_as_member_immediately: bool,
    },
    /// Change the role of a cache member
    Edit {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Username of the user
        user: String,
        /// Role name, such as `Admin`, `Write`, `View`, or a custom role.
        role: String,
    },
    /// Remove a member from the cache.
    Remove {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Username of the user
        user: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CacheInvitationCommands {
    /// Display pending invitations for the cache.
    List {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Revoke a user's pending invitation
    Revoke {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Username of the user
        user: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CacheRoleCommand {
    /// Show the roles of the cache and the permissions that custom roles can use.
    List {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Show one role with its permissions.
    Show {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Role ID
        role_id: String,
    },
    /// Create a custom role.
    Create {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Role name
        name: String,
        /// Comma-separated permissions. See `gradient cache role list`.
        #[arg(long, value_delimiter = ',', required = true)]
        role_permission_names: Vec<String>,
    },
    /// Change the name or permissions of a custom role.
    Edit {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Role ID
        role_id: String,
        /// New name for the role
        #[arg(long)]
        name: Option<String>,
        /// Comma-separated permissions. See `gradient cache role list`.
        #[arg(long, value_delimiter = ',')]
        role_permission_names: Option<Vec<String>>,
    },
    /// Delete a custom role
    Delete {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Role ID
        role_id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CacheTeamCommand {
    /// List the teams that can access the cache
    List {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Grant a team access to the cache with a role.
    Add {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Team name
        team: String,
        /// Role name, such as `Admin`, `Write`, `View`, or a custom role.
        role: String,
    },
    /// Change the role of a team on the cache
    Edit {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Team name
        team: String,
        /// Role name, such as `Admin`, `Write`, `View`, or a custom role.
        role: String,
    },
    /// Revoke a team's access to a cache
    Remove {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Team name
        team: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CacheRequestSubcommand {
    /// Display projects that request a cache subscription.
    List {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
    },
    /// Approve requests for a project subscription.
    Approve {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Project Name
        project: String,
    },
    /// Reject the subscription request for the project.
    Deny {
        /// Cache name
        #[arg(add = ArgValueCompleter::new(completion::complete_caches))]
        cache: String,
        /// Project Name
        project: String,
    },
}

pub async fn handle_cache_user_commands(cmd: CacheUserCommands, out: Output) {
    let client = client_from_config(out);
    let caches = client.caches();
    match cmd {
        CacheUserCommands::List { cache } => match caches.list_members(&cache).await {
            Ok(members) => {
                out.ok(&members);
                if members.is_empty() {
                    out.human("The cache has no members.");
                }
                for member in &members {
                    match &member.user_display_name {
                        Some(display_name) => {
                            out.human(format!("{} ({display_name})", member.name))
                        }
                        None => out.human(&member.name),
                    }
                }
            }
            Err(e) => out.fail(e),
        },
        CacheUserCommands::Add {
            cache,
            user,
            role,
            add_as_member_immediately,
        } => {
            match caches
                .invite_cache_member(
                    &cache,
                    AddMemberRequest {
                        user,
                        role,
                        add_as_member_immediately: add_as_member_immediately.then_some(true),
                    },
                )
                .await
            {
                Ok(message) => {
                    out.ok(&serde_json::json!({
                        "invited": !add_as_member_immediately,
                        "message": message,
                    }));
                    out.human(format!("{message}."));
                }
                Err(e) => out.fail(e),
            }
        }
        CacheUserCommands::Edit { cache, user, role } => {
            match caches
                .update_cache_member_role(&cache, ChangeMemberRoleRequest { user, role })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Updated the role.");
                }
                Err(e) => out.fail(e),
            }
        }
        CacheUserCommands::Remove { cache, user } => {
            match caches
                .delete_cache_member(&cache, RemoveMemberRequest { user })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"removed": true}));
                    out.human("Removed member.");
                }
                Err(e) => out.fail(e),
            }
        }
    }
}

pub async fn apply_cache_invitation_commands(cmd: CacheInvitationCommands, out: Output) {
    let client = client_from_config(out);
    let caches = client.caches();
    match cmd {
        CacheInvitationCommands::List { cache } => {
            match caches.list_pending_invitations(&cache).await {
                Ok(invitations) => {
                    out.ok(&invitations);
                    if invitations.is_empty() {
                        out.human("No pending invitations.");
                    }
                    for invitation in &invitations {
                        out.human(format!(
                            "{} ({}): {}, expires {}",
                            invitation.name,
                            invitation.user,
                            invitation.role,
                            invitation.expires_at
                        ));
                    }
                }
                Err(e) => out.fail(e),
            }
        }
        CacheInvitationCommands::Revoke { cache, user } => {
            match caches
                .revoke_pending_invitation(&cache, RevokeInvitationRequest { user })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"revoked": true}));
                    out.human("Invitation revoked.");
                }
                Err(e) => out.fail(e),
            }
        }
    }
}

pub async fn apply_cache_role_command(cmd: CacheRoleCommand, out: Output) {
    let client = client_from_config(out);
    let caches = client.caches();
    let print_role = |role: &RoleResponseCacheRoleOwner| {
        out.human(format!(
            "{} {}: {}",
            role.id.0,
            role.name,
            role.permissions.join(",")
        ))
    };
    match cmd {
        CacheRoleCommand::List { cache } => {
            match caches.list_roles_and_available_permissions(&cache).await {
                Ok(roles) => {
                    out.ok(&roles);
                    roles.roles.iter().for_each(print_role);
                    let permissions: Vec<&str> = roles
                        .available_permissions
                        .iter()
                        .map(|permission| permission.id.as_str())
                        .collect();
                    out.human(format!("Available permissions: {}", permissions.join(",")));
                }
                Err(e) => out.fail(e),
            }
        }
        CacheRoleCommand::Show { cache, role_id } => {
            match caches.fetch_cache_role(&cache, &role_id).await {
                Ok(role) => {
                    out.ok(&role);
                    print_role(&role);
                }
                Err(e) => out.fail(e),
            }
        }
        CacheRoleCommand::Create {
            cache,
            name,
            role_permission_names,
        } => {
            let body = CreateRoleRequest {
                name: IndexName(name),
                permissions: role_permission_names,
            };
            match caches.create_custom_role(&cache, body).await {
                Ok(role) => {
                    out.ok(&role);
                    out.human("Created a role.");
                    print_role(&role);
                }
                Err(e) => out.fail(e),
            }
        }
        CacheRoleCommand::Edit {
            cache,
            role_id,
            name,
            role_permission_names,
        } => {
            let body = PatchRoleRequest {
                name: name.map(IndexName),
                permissions: role_permission_names,
            };
            match caches.update_custom_role(&cache, &role_id, body).await {
                Ok(role) => {
                    out.ok(&role);
                    out.human("Updated the role.");
                    print_role(&role);
                }
                Err(e) => out.fail(e),
            }
        }
        CacheRoleCommand::Delete { cache, role_id } => {
            match caches.delete_custom_role(&cache, &role_id).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Role deleted.");
                }
                Err(e) => out.fail(e),
            }
        }
    }
}

pub async fn execute_cache_team_command(cmd: CacheTeamCommand, out: Output) {
    let client = client_from_config(out);
    let caches = client.caches();
    match cmd {
        CacheTeamCommand::List { cache } => {
            match caches.list_teams_granted_access_to_cache(&cache).await {
                Ok(grants) => {
                    out.ok(&grants);
                    if grants.is_empty() {
                        out.human("No team can access this cache.");
                    }
                    for grant in &grants {
                        out.human(format!(
                            "{} ({}): {}{}",
                            grant.team,
                            grant.display_name,
                            grant.role.as_deref().unwrap_or("-"),
                            if grant.pending { " [Pending]" } else { "" }
                        ));
                    }
                }
                Err(e) => out.fail(e),
            }
        }
        CacheTeamCommand::Add { cache, team, role } => {
            match caches
                .grant_team_access_to_cache(&cache, GrantCacheRequest { team, role })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"added": true}));
                    out.human("Added a team.");
                }
                Err(e) => out.fail(e),
            }
        }
        CacheTeamCommand::Edit { cache, team, role } => {
            match caches
                .update_cache_team_grant_role(&cache, &team, PatchCacheGrantRequest { role })
                .await
            {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Updated the role.");
                }
                Err(e) => out.fail(e),
            }
        }
        CacheTeamCommand::Remove { cache, team } => {
            match caches.remove_team_grant(&cache, &team).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"removed": true}));
                    out.human("Team deleted.");
                }
                Err(e) => out.fail(e),
            }
        }
    }
}

pub async fn respond_to_subscription_cache_requests(cmd: CacheRequestSubcommand, out: Output) {
    let client = client_from_config(out);
    let caches = client.caches();
    match cmd {
        CacheRequestSubcommand::List { cache } => {
            match caches.list_subscription_requests(&cache).await {
                Ok(requests) => {
                    out.ok(&requests);
                    if requests.is_empty() {
                        out.human("No pending subscription requests.");
                    }
                    for request in &requests {
                        out.human(format!(
                            "{} ({}): {} by {}",
                            request.project,
                            request.project_display_name,
                            request.mode,
                            request.requested_by
                        ));
                    }
                }
                Err(e) => out.fail(e),
            }
        }
        CacheRequestSubcommand::Approve { cache, project } => {
            match caches.approve_subscription_request(&cache, &project).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"approved": true}));
                    out.human("The request received approval.");
                }
                Err(e) => out.fail(e),
            }
        }
        CacheRequestSubcommand::Deny { cache, project } => {
            match caches.deny_subscription_request(&cache, &project).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"denied": true}));
                    out.human("Request rejected.");
                }
                Err(e) => out.fail(e),
            }
        }
    }
}
