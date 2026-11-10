/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::webhook::{WebhookSubcommands, run_webhook_subcommand};
use crate::input::{CommandLineFormField, client_from_config, confirm_delete, handle_input};
use crate::output::{ExitKind, Output};
use clap::{Args, Subcommand};
use connector::api::{
    MakeTeamRequest, PatchTeamRequest, PatchWorkerRequest, RegisterWorkerRequest,
    RemoveTeamMemberRequest, RevokeInvitationRequest, TeamMemberRequest, TeamRole,
};
use connector::webhooks::WebhookResource;

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List the teams for a user and the role in each.
    List,
    /// Display the team settings and the role of the current user in them.
    Show {
        /// Team name
        team: String,
    },
    /// Create a team and become its administrator. Empty fields open in $EDITOR.
    Create {
        /// A unique team name for URLs and commands
        #[arg(short, long)]
        name: Option<String>,
        /// Name shown in the web interface
        #[arg(short, long)]
        display_name: Option<String>,
    },
    /// Edit settings for a team. Only superusers can change OIDC groups, SCIM groups and new project settings.
    Edit {
        /// Team name
        team: String,
        /// Name shown in the web interface
        #[arg(short, long)]
        display_name: Option<String>,
        /// Projects that team members create give team members access with the new project role.
        #[arg(long)]
        new_project_users: Option<bool>,
        /// Grant team workers for each new project.
        #[arg(long)]
        new_project_workers: Option<bool>,
        /// Project role for team members in projects that they create.
        #[arg(long)]
        new_project_role: Option<String>,
        /// OIDC group for a user to join the team
        #[arg(long)]
        oidc_group: Option<String>,
        /// SCIM group whose users become team members
        #[arg(long)]
        scim_group: Option<String>,
    },
    /// Delete a team along with its grants, grant requests, invitations and workers
    Delete {
        /// Team name
        team: String,
        /// Delete without a confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Manage team members
    Members {
        #[command(subcommand)]
        cmd: TeamMembersCommands,
    },
    /// Manage invitations that invited users haven't accepted yet.
    PendingInvitations {
        #[command(subcommand)]
        cmd: TeamPendingInvitationsCommand,
    },
    /// Approve or deny requests from projects or caches to grant access to the team.
    GrantRequests {
        #[command(subcommand)]
        cmd: TeamGrantRequestsCommand,
    },
    /// List the projects and caches that grant access to the team.
    ProjectsAndCaches {
        /// Team name
        team: String,
    },
    /// Manage team workers. Team workers build for all projects that grant access to the team of the worker.
    Worker {
        #[command(subcommand)]
        cmd: TeamWorkerSubcommand,
    },
    /// Manage team webhooks
    Webhook {
        #[command(subcommand)]
        cmd: WebhookSubcommands<TeamWebhookTeamNameArguments>,
    },
}

#[derive(Args, Debug)]
pub struct TeamWebhookTeamNameArguments {
    /// Team name
    team: String,
}

#[derive(Subcommand, Debug)]
pub enum TeamMembersCommands {
    /// List the members of a team with their role
    List {
        /// Team name
        team: String,
    },
    /// Invite a user to a team. The user can join after acceptance, while superusers add users immediately.
    Add {
        /// Team name
        team: String,
        /// Username
        user: String,
        /// Role in the Team
        #[arg(short, long, default_value = "member")]
        role: TeamRole,
    },
    /// Change the role of a team member
    Edit {
        /// Team name
        team: String,
        /// Username
        user: String,
        /// Role in the Team
        #[arg(short, long)]
        role: TeamRole,
    },
    /// Remove a member from a team
    Remove {
        /// Team name
        team: String,
        /// Username
        user: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TeamPendingInvitationsCommand {
    /// List the team invitations that await a response from invited users.
    List {
        /// Team name
        team: String,
    },
    /// Withdraw the pending invitation for a user
    Withdraw {
        /// Team name
        team: String,
        /// Username
        user: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TeamGrantRequestsCommand {
    /// List a team's grant requests, newest first.
    List {
        /// Team name
        team: String,
    },
    /// Approve a grant request and enable the grant
    Approve {
        /// Team name
        team: String,
        /// Grant request ID (as shown in `gradient team grant-requests list`)
        request_id: uuid::Uuid,
    },
    /// Reject grant request
    Reject {
        /// Team name
        team: String,
        /// Grant request ID (as shown in `gradient team grant-requests list`)
        request_id: uuid::Uuid,
    },
}

#[derive(Args, Debug)]
pub struct WorkerCapabilityToggles {
    /// Allow or deny network access for jobs on a worker.
    #[arg(long)]
    enable_network: Option<bool>,
    /// Allow or deny the delivery of secrets to workers
    #[arg(long)]
    enable_secrets: Option<bool>,
    /// Allow or deny workers to run programs with `gradient run`
    #[arg(long)]
    enable_exec: Option<bool>,
    /// Allow or deny flake evaluation on workers.
    #[arg(long)]
    enable_eval: Option<bool>,
    /// Allow or deny workers to build derivations.
    #[arg(long)]
    enable_build: Option<bool>,
}

#[derive(Subcommand, Debug)]
pub enum TeamWorkerSubcommand {
    /// List the workers of a team and show their connection status.
    List {
        /// Team name
        team: String,
    },
    /// Register a worker with a team. The worker can answer the challenge from the server with `<team id>:<token>` from the peers file.
    Register {
        /// Team name
        team: String,
        /// Worker ID
        worker_id: uuid::Uuid,
        /// Name shown in the web interface
        #[arg(short = 'n', long)]
        display_name: String,
        /// Optional WebSocket URL the worker is listening on for incoming server connections
        #[arg(short, long)]
        url: Option<String>,
        /// Pre-generated token (output of `openssl rand -base64 48`). The server is generating and
        /// printing one when omitted. Store it securely, it cannot be retrieved again.
        #[arg(short, long)]
        token: Option<String>,
        #[command(flatten)]
        capabilities: WorkerCapabilityToggles,
    },
    /// Modify the settings for a team worker. Disabling a worker can abort running jobs.
    Edit {
        /// Team name
        team: String,
        /// Worker ID
        worker_id: String,
        /// Name shown in the web interface
        #[arg(short = 'n', long)]
        display_name: Option<String>,
        /// Activate or deactivate a worker
        #[arg(long)]
        active: Option<bool>,
        #[command(flatten)]
        capabilities: WorkerCapabilityToggles,
    },
    /// Delete a worker from a team
    Delete {
        /// Team name
        team: String,
        /// Worker ID
        worker_id: String,
        /// Delete without a confirmation prompt
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show what pending evaluations need but no connected worker can provide: build systems and features, network access, secrets, evaluations.
    MissingWorkerCapabilities {
        /// Team name
        team: String,
        /// Comma-separated project names or IDs to restrict which projects count toward totals.
        #[arg(long)]
        projects: Option<String>,
    },
}

pub async fn handle(cmd: Commands, out: Output) {
    let client = client_from_config(out);
    let teams = client.teams();
    match cmd {
        Commands::List => match teams.list().await {
            Ok(list) => {
                out.ok(&list);
                if list.is_empty() {
                    out.human("Not a member of any team.");
                }
                for team in list {
                    out.human(format!(
                        "{} ({}): {}",
                        team.name, team.display_name, team.role
                    ));
                }
            }
            Err(e) => out.fail(e),
        },

        Commands::Show { team } => match teams.get(&team).await {
            Ok(team) => {
                out.ok(&team);
                out.human(format!("Name:                {}", team.name));
                out.human(format!("Display name:        {}", team.display_name));
                out.human(format!("ID:                  {}", team.id));
                out.human(format!(
                    "Role:                {}",
                    team.role.map_or("-".to_owned(), |role| role.to_string())
                ));
                out.human(format!("Managed:             {}", team.managed));
                out.human(format!("New project users:   {}", team.new_project_users));
                out.human(format!("New project workers: {}", team.new_project_workers));
                out.human(format!(
                    "New project role:    {}",
                    team.new_project_role.as_deref().unwrap_or("-")
                ));
                out.human(format!(
                    "OIDC group:          {}",
                    team.oidc_group.as_deref().unwrap_or("-")
                ));
                out.human(format!(
                    "SCIM group:          {}",
                    team.scim_group.as_deref().unwrap_or("-")
                ));
            }
            Err(e) => out.fail(e),
        },

        Commands::Create { name, display_name } => {
            let input = handle_input(
                vec![
                    CommandLineFormField::new_non_empty("Name", name),
                    CommandLineFormField::new_non_empty("Display Name", display_name),
                ],
                false,
                out,
            );
            let name = input.get("Name");
            match teams
                .create(MakeTeamRequest {
                    name: name.clone().into(),
                    display_name: input.get("Display Name").into(),
                })
                .await
            {
                Ok(created) => {
                    out.ok(&created);
                    out.progress("Created team.");
                    out.human(created.id);
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Edit {
            team,
            display_name,
            new_project_users,
            new_project_workers,
            new_project_role,
            oidc_group,
            scim_group,
        } => {
            let body = PatchTeamRequest {
                display_name: display_name.map(Into::into),
                new_project_users,
                new_project_workers,
                new_project_role,
                oidc_group,
                scim_group,
            };
            if body == PatchTeamRequest::default() {
                out.err(ExitKind::Usage, "Specify at least one setting to change.");
            }
            match teams.update(&team, body).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"updated": true}));
                    out.human("Team updated.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Delete { team, yes } => {
            if !confirm_delete(&format!("Delete team '{team}'?"), yes, out) {
                return;
            }
            match teams.delete(&team).await {
                Ok(_) => {
                    out.ok(&serde_json::json!({"deleted": true}));
                    out.human("Team deleted.");
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Members { cmd } => handle_members(cmd, &client, out).await,
        Commands::PendingInvitations { cmd } => handle_invitations(cmd, &client, out).await,
        Commands::GrantRequests { cmd } => handle_grant_requests(cmd, &client, out).await,

        Commands::ProjectsAndCaches { team } => {
            match teams.list_projects_and_caches_granting_team(&team).await {
                Ok(grants) => {
                    out.ok(&grants);
                    if grants.projects.is_empty() && grants.caches.is_empty() {
                        out.human("No projects or caches grant access to this team.");
                    }
                    for project in grants.projects {
                        out.human(format!(
                            "project {} ({}): role {}, users {}, workers {}",
                            project.project,
                            project.display_name,
                            project.role.as_deref().unwrap_or("-"),
                            project.users,
                            project.workers
                        ));
                    }
                    for cache in grants.caches {
                        out.human(format!(
                            "cache {} ({}): role {}",
                            cache.cache,
                            cache.display_name,
                            cache.role.as_deref().unwrap_or("-")
                        ));
                    }
                }
                Err(e) => out.fail(e),
            }
        }

        Commands::Worker { cmd } => handle_workers(cmd, &client, out).await,
        Commands::Webhook { cmd } => {
            run_webhook_subcommand(cmd, |owner| WebhookResource::Team(owner.team.clone()), out)
                .await
        }
    }
}

async fn handle_members(cmd: TeamMembersCommands, client: &connector::Client, out: Output) {
    let teams = client.teams();
    let result = match cmd {
        TeamMembersCommands::List { team } => match teams.list_members(&team).await {
            Ok(members) => {
                out.ok(&members);
                if members.is_empty() {
                    out.human("No members in this team.");
                }
                for member in members {
                    out.human(format!(
                        "{} ({}): {} [{}]",
                        member.user, member.name, member.role, member.source
                    ));
                }
                return;
            }
            Err(e) => out.fail(e),
        },
        TeamMembersCommands::Add { team, user, role } => teams
            .add_user(&team, TeamMemberRequest { user, role })
            .await
            .map(|message| (serde_json::json!({"message": message}), message)),
        TeamMembersCommands::Edit { team, user, role } => teams
            .update_team_member_role(&team, TeamMemberRequest { user, role })
            .await
            .map(|_| (serde_json::json!({"updated": true}), "Role changed.".into())),
        TeamMembersCommands::Remove { team, user } => teams
            .remove_team_member(&team, RemoveTeamMemberRequest { user })
            .await
            .map(|_| {
                (
                    serde_json::json!({"removed": true}),
                    "Removed the member.".into(),
                )
            }),
    };
    report(result, out);
}

async fn handle_invitations(
    cmd: TeamPendingInvitationsCommand,
    client: &connector::Client,
    out: Output,
) {
    let teams = client.teams();
    match cmd {
        TeamPendingInvitationsCommand::List { team } => {
            match teams.list_pending_invitations(&team).await {
                Ok(invitations) => {
                    out.ok(&invitations);
                    if invitations.is_empty() {
                        out.human("No pending invitations.");
                    }
                    for invitation in invitations {
                        out.human(format!(
                            "{} ({}): {}, expires {}",
                            invitation.user,
                            invitation.name,
                            invitation.role,
                            invitation.expires_at
                        ));
                    }
                }
                Err(e) => out.fail(e),
            }
        }
        TeamPendingInvitationsCommand::Withdraw { team, user } => report(
            teams
                .withdraw_team_invitation(&team, RevokeInvitationRequest { user })
                .await
                .map(|_| {
                    (
                        serde_json::json!({"withdrawn": true}),
                        "Invitation revoked.".into(),
                    )
                }),
            out,
        ),
    }
}

async fn handle_grant_requests(
    cmd: TeamGrantRequestsCommand,
    client: &connector::Client,
    out: Output,
) {
    let teams = client.teams();
    match cmd {
        TeamGrantRequestsCommand::List { team } => {
            match teams.list_team_grant_requests(&team).await {
                Ok(requests) => {
                    out.ok(&requests);
                    if requests.is_empty() {
                        out.human("No grant requests.");
                    }
                    for request in requests {
                        out.human(format!(
                            "{} {} {} ({}): role {}, users {}, workers {}, by {}",
                            request.id,
                            request.kind,
                            request.target,
                            request.display_name,
                            request.role.as_deref().unwrap_or("-"),
                            request.users,
                            request.workers,
                            request.requested_by.as_deref().unwrap_or("-")
                        ));
                    }
                }
                Err(e) => out.fail(e),
            }
        }
        TeamGrantRequestsCommand::Approve { team, request_id } => report(
            teams
                .approve_grant_request(&team, &request_id.to_string())
                .await
                .map(|_| {
                    (
                        serde_json::json!({"approved": true}),
                        "Grant request approved.".into(),
                    )
                }),
            out,
        ),
        TeamGrantRequestsCommand::Reject { team, request_id } => report(
            teams
                .reject_team_grant_request(&team, &request_id.to_string())
                .await
                .map(|_| {
                    (
                        serde_json::json!({"rejected": true}),
                        "Grant request denied.".into(),
                    )
                }),
            out,
        ),
    }
}

async fn handle_workers(cmd: TeamWorkerSubcommand, client: &connector::Client, out: Output) {
    let teams = client.teams();
    match cmd {
        TeamWorkerSubcommand::List { team } => match teams.list_team_workers(&team).await {
            Ok(workers) => {
                out.ok(&workers);
                if workers.is_empty() {
                    out.human("No workers registered.");
                }
                for worker in workers {
                    let status = match (worker.active, worker.connected) {
                        (false, _) => "inactive",
                        (true, true) => "online",
                        (true, false) => "offline",
                    };
                    let url = worker
                        .url
                        .map(|url| format!(" (url: {url})"))
                        .unwrap_or_default();
                    out.human(format!(
                        "{} \"{}\": {} [{status}]{url}",
                        worker.worker_id, worker.display_name, worker.registered_at
                    ));
                }
            }
            Err(e) => out.fail(e),
        },

        TeamWorkerSubcommand::Register {
            team,
            worker_id,
            display_name,
            url,
            token,
            capabilities,
        } => {
            let body = RegisterWorkerRequest {
                worker_id: worker_id.to_string(),
                display_name,
                url,
                token,
                enable_network: capabilities.enable_network,
                enable_secrets: capabilities.enable_secrets,
                enable_exec: capabilities.enable_exec,
                enable_eval: capabilities.enable_eval,
                enable_build: capabilities.enable_build,
            };
            match teams.register_team_worker(&team, body).await {
                Ok(registered) => {
                    out.ok(&registered);
                    out.human("Worker registered.");
                    out.human(format!("Team ID:  {}", registered.team));
                    if let Some(token) = registered.token {
                        out.human(format!("Token:    {token}"));
                        out.human("Store the token securely. It cannot be shown again.");
                    }
                }
                Err(e) => out.fail(e),
            }
        }

        TeamWorkerSubcommand::Edit {
            team,
            worker_id,
            display_name,
            active,
            capabilities,
        } => {
            let body = PatchWorkerRequest {
                active,
                display_name,
                enable_network: capabilities.enable_network,
                enable_secrets: capabilities.enable_secrets,
                enable_exec: capabilities.enable_exec,
                enable_eval: capabilities.enable_eval,
                enable_build: capabilities.enable_build,
            };
            if body == PatchWorkerRequest::default() {
                out.err(ExitKind::Usage, "Specify at least one setting to change.");
            }
            report(
                teams
                    .update_team_worker(&team, &worker_id, body)
                    .await
                    .map(|_| {
                        (
                            serde_json::json!({"updated": true}),
                            "Worker updated.".into(),
                        )
                    }),
                out,
            );
        }

        TeamWorkerSubcommand::Delete {
            team,
            worker_id,
            yes,
        } => {
            if !confirm_delete(
                &format!("Remove worker {worker_id} from team '{team}'?"),
                yes,
                out,
            ) {
                return;
            }
            report(
                teams.delete_team_worker(&team, &worker_id).await.map(|_| {
                    (
                        serde_json::json!({"deleted": true}),
                        "Deleted worker.".into(),
                    )
                }),
                out,
            )
        }

        TeamWorkerSubcommand::MissingWorkerCapabilities { team, projects } => {
            match teams
                .get_team_workers_demand_missing_capabilities(&team, projects.as_deref())
                .await
            {
                Ok(demand) => {
                    out.ok(&demand);
                    if demand.builds.is_empty()
                        && demand.network == 0
                        && demand.secrets == 0
                        && demand.eval == 0
                    {
                        out.human("A worker can process every queued evaluation.");
                        return;
                    }
                    for build in &demand.builds {
                        let features = if build.required_features.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", build.required_features.join(", "))
                        };
                        out.human(format!(
                            "builds {}{features}: {}",
                            build.architecture, build.build_count
                        ));
                    }
                    out.human(format!("network: {}", demand.network));
                    out.human(format!("secrets: {}", demand.secrets));
                    out.human(format!("eval:    {}", demand.eval));
                }
                Err(e) => out.fail(e),
            }
        }
    }
}

fn report(result: Result<(serde_json::Value, String), connector::ConnectorError>, out: Output) {
    match result {
        Ok((json, message)) => {
            out.ok(&json);
            out.human(message);
        }
        Err(e) => out.fail(e),
    }
}
