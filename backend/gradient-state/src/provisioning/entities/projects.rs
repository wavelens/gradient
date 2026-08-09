/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::StateApplicator;
use super::super::{DynError, PendingProjectMembership, PendingProjectMemberships};
use super::super::{derive_public_key, lookup_id, read_credential};
use crate::config::*;
use anyhow::Result;
use gradient_entity::*;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::collections::{HashMap, HashSet};

impl<'a> StateApplicator<'a> {
    // ── apply_projects_without_members ───────────────────────────────────

    /// Create/update the `project` row. Membership reconciliation happens
    /// later in `apply_project_members`, after `apply_roles` so custom project
    /// roles referenced by `members` can be resolved against rows inserted in
    /// the same apply pass.
    pub(crate) async fn apply_projects_without_members(
        &self,
        state_projects: &HashMap<String, StateProject>,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;

        for state_project in state_projects.values() {
            let (private_key, _) = read_credential(
                "project",
                &state_project.name,
                "private_key",
                "private key file",
            )?;
            let private_key = private_key.trim();

            let public_key = derive_public_key(private_key)?;
            let encrypted_private_key = self.encrypt_to_b64(private_key, "SSH private key")?;

            let created_by_id = lookup_id(&user_map, &state_project.created_by, "User")?;

            let existing_project = project::Entity::find()
                .filter(project::Column::Name.eq(&state_project.name))
                .one(self.db)
                .await?;

            let now = now();

            let declared_id = match &state_project.id {
                Some(s) => Some(s.trim().parse::<ProjectId>().map_err(|e| {
                    format!(
                        "Project '{}' has an invalid id '{}': {}",
                        state_project.name, s, e
                    )
                })?),
                None => None,
            };

            let _project_id = if let Some(existing) = existing_project {
                let project_id = existing.id;
                if let Some(declared) = declared_id
                    && declared != project_id
                {
                    return Err(format!(
                        "Project '{}' already exists with id {} but state declares id {}; the id is immutable",
                        state_project.name, project_id, declared
                    )
                    .into());
                }
                let mut project: project::ActiveModel = existing.into();
                project.display_name = Set(state_project.display_name.clone());
                project.description = Set(state_project.description.clone().unwrap_or_default());
                project.public_key = Set(public_key);
                project.private_key = Set(encrypted_private_key.clone());
                project.created_by = Set(created_by_id);
                project.public = Set(state_project.public);
                project.hide_build_requests = Set(state_project.hide_build_requests);
                project.managed = Set(true);
                project.update(self.db).await?;
                tracing::info!(name = %state_project.name, "Updated managed project");
                project_id
            } else {
                let project_id = declared_id.unwrap_or_else(ProjectId::now_v7);
                let project = project::Model {
                    id: project_id,
                    name: state_project.name.clone(),
                    display_name: state_project.display_name.clone(),
                    description: state_project.description.clone().unwrap_or_default(),
                    public_key,
                    private_key: encrypted_private_key,
                    public: state_project.public,
                    hide_build_requests: state_project.hide_build_requests,
                    created_by: created_by_id,
                    created_at: now,
                    managed: true,
                }
                .into_active_model();

                project.insert(self.db).await?;
                tracing::info!(name = %state_project.name, "Created managed project");
                project_id
            };
        }

        Ok(())
    }

    // ── apply_project_members ────────────────────────────────────────────

    /// Reconcile `project_user` rows for every state-managed project.
    ///
    /// When `state_project.members` is empty, the legacy behavior applies:
    /// `created_by` is added as Admin if no row exists. When `members` is
    /// non-empty, the declared list is authoritative - see
    /// [`StateApplicator::apply_members_for_project`] for the per-project logic.
    pub(crate) async fn apply_project_members(
        &self,
        state_projects: &HashMap<String, StateProject>,
        pending: &mut PendingProjectMemberships,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;
        let project_map = self.project_lookup().await?;

        for state_project in state_projects.values() {
            let project_id = lookup_id(&project_map, &state_project.name, "Project")?;
            let created_by_id = lookup_id(&user_map, &state_project.created_by, "User")?;

            if state_project.members.is_empty() {
                let existing = project_user::Entity::find()
                    .filter(project_user::Column::Project.eq(project_id))
                    .filter(project_user::Column::User.eq(created_by_id))
                    .one(self.db)
                    .await?;

                if existing.is_none() {
                    project_user::Model {
                        id: ProjectUserId::now_v7(),
                        project: project_id,
                        user: created_by_id,
                        role: BASE_ROLE_ADMIN_ID,
                    }
                    .into_active_model()
                    .insert(self.db)
                    .await?;
                    tracing::info!(
                        username = %state_project.created_by,
                        project = %state_project.name,
                        "Added admin member to project"
                    );
                }
            } else {
                self.apply_members_for_project(
                    project_id,
                    &state_project.name,
                    &state_project.members,
                    pending,
                )
                .await
                .map_err(|e| {
                    format!(
                        "Failed to apply members for project '{}': {}",
                        state_project.name, e
                    )
                })?;
            }
        }

        Ok(())
    }

    /// Reconcile membership for a single state-managed project whose
    /// `members` list is non-empty.
    ///
    /// - Missing users are recorded into `pending` and skipped (issue #94);
    ///   they'll be applied when the user later registers or signs in via
    ///   OIDC.
    /// - Built-in roles (`Admin`/`Write`/`View`) map to constant role IDs;
    ///   custom project roles resolve against `role` rows scoped to this project.
    /// - Drift: existing memberships not in the declared user set are
    ///   deleted. State owns the membership list when explicitly declared.
    pub(crate) async fn apply_members_for_project(
        &self,
        project_id: ProjectId,
        project_name: &str,
        members: &[StateProjectMemberEntry],
        pending: &mut PendingProjectMemberships,
    ) -> Result<(), DynError> {
        let user_map = self.user_lookup().await?;

        let custom_roles: HashMap<String, RoleId> = role::Entity::find()
            .filter(role::Column::Project.eq(project_id))
            .filter(role::Column::Managed.eq(true))
            .all(self.db)
            .await?
            .into_iter()
            .map(|r| (r.name, r.id))
            .collect();

        let mut declared_user_ids: HashSet<UserId> = HashSet::new();

        for member in members {
            let role_id = match member.role.as_str() {
                "Admin" => BASE_ROLE_ADMIN_ID,
                "Write" => BASE_ROLE_WRITE_ID,
                "View" => BASE_ROLE_VIEW_ID,
                name => *custom_roles.get(name).ok_or_else(|| -> DynError {
                    format!(
                        "Project '{}' member '{}' references unknown role '{}'",
                        project_name, member.user, name
                    )
                    .into()
                })?,
            };

            match user_map.get(&member.user).copied() {
                Some(user_id) => {
                    declared_user_ids.insert(user_id);
                    let existing = project_user::Entity::find()
                        .filter(project_user::Column::Project.eq(project_id))
                        .filter(project_user::Column::User.eq(user_id))
                        .one(self.db)
                        .await?;
                    if let Some(row) = existing {
                        if row.role != role_id {
                            let mut active: project_user::ActiveModel = row.into();
                            active.role = Set(role_id);
                            active.update(self.db).await?;
                            tracing::info!(
                                project = %project_name,
                                user = %member.user,
                                "Updated project membership role"
                            );
                        }
                    } else {
                        project_user::Model {
                            id: ProjectUserId::now_v7(),
                            project: project_id,
                            user: user_id,
                            role: role_id,
                        }
                        .into_active_model()
                        .insert(self.db)
                        .await?;
                        tracing::info!(
                            project = %project_name,
                            user = %member.user,
                            "Added project member"
                        );
                    }
                }
                None => {
                    tracing::info!(
                        project = %project_name,
                        user = %member.user,
                        "Declared member not yet registered; deferring until user creation"
                    );
                    pending.entry(member.user.clone()).or_default().push(
                        PendingProjectMembership {
                            project: project_id,
                            role: role_id,
                        },
                    );
                }
            }
        }

        let existing = project_user::Entity::find()
            .filter(project_user::Column::Project.eq(project_id))
            .all(self.db)
            .await?;
        for row in existing {
            if !declared_user_ids.contains(&row.user) {
                let user_id = row.user;
                project_user::Entity::delete_by_id(row.id)
                    .exec(self.db)
                    .await?;
                tracing::info!(
                    project = %project_name,
                    %user_id,
                    "Removed project member no longer in state"
                );
            }
        }

        Ok(())
    }
}
