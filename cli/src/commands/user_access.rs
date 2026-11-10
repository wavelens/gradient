/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::client_from_config;
use crate::output::Output;
use clap::Subcommand;
use connector::api::{AccessReason, PersonAccessReason, UserAccessSummary};

#[derive(Subcommand, Debug)]
pub enum UserAccessCommands {
    /// Show the projects, caches and teams a user reaches. Without a name, shows your own access. Other users need an instance admin.
    Access {
        /// Username (default: you)
        name: Option<String>,
    },
}

pub async fn handle(cmd: UserAccessCommands, out: Output) {
    match cmd {
        UserAccessCommands::Access { name } => {
            let client = client_from_config(out);
            let summary = match name {
                Some(name) => client.user().get_user_access_summary(&name).await,
                None => client.user().get_own_access_summary().await,
            };
            match summary {
                Ok(summary) => {
                    out.ok(&summary);
                    print_user_access_summary_lines(&summary, out);
                }
                Err(e) => out.fail(e),
            }
        }
    }
}

pub fn access_reasons_to_comma_separated_text(reasons: &[PersonAccessReason]) -> String {
    reasons
        .iter()
        .map(|reason| {
            let team = reason
                .team
                .as_ref()
                .map(|team| team.name.as_str())
                .unwrap_or("-");
            let mut text = match reason.kind {
                AccessReason::Direct => "direct".to_string(),
                AccessReason::Team => format!("team {team}"),
                AccessReason::Owner => format!("owner team {team}"),
                AccessReason::Superuser => "instance admin".to_string(),
            };
            if let (AccessReason::Team | AccessReason::Owner, Some(role)) =
                (reason.kind, reason.role.as_deref())
            {
                text.push_str(&format!(": {role}"));
            }
            if !reason.applies {
                text.push_str(" (overridden)");
            }
            if reason.managed {
                text.push_str(" (server settings)");
            }
            text
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_user_access_summary_lines(summary: &UserAccessSummary, out: Output) {
    if summary.superuser {
        out.human("This user is an instance admin and can access all projects, caches and teams.");
    }
    if summary.projects.is_empty() && summary.caches.is_empty() && summary.teams.is_empty() {
        if !summary.superuser {
            out.human("No access.");
        }
        return;
    }
    for (heading, rows) in [("Projects", &summary.projects), ("Cache", &summary.caches)] {
        if rows.is_empty() {
            continue;
        }
        out.human(heading);
        for row in rows {
            out.human(format!(
                "  {}: {} ({})",
                row.name,
                row.role.as_deref().unwrap_or("-"),
                access_reasons_to_comma_separated_text(&row.reasons)
            ));
        }
    }
    if !summary.teams.is_empty() {
        out.human("Teams");
        for team in &summary.teams {
            out.human(format!("  {}: {}", team.name, team.role));
        }
    }
}
