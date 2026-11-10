/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::lookup_id;
use crate::config::*;
use anyhow::Result;
use gradient_entity::*;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_roles(
        &self,
        state_roles: &HashMap<String, StateRole>,
    ) -> Result<(), DynError> {
        let project_lookup = self.project_lookup().await?;

        for state_role in state_roles.values() {
            let project_id = lookup_id(&project_lookup, &state_role.project, "Project")?;

            let mut perms = Vec::with_capacity(state_role.permissions.len());
            for wire in &state_role.permissions {
                let p = gradient_types::permissions::Permission::from_wire_name(wire).ok_or_else(
                    || {
                        format!(
                            "Role '{}' references unknown permission '{}'",
                            state_role.name, wire
                        )
                    },
                )?;
                perms.push(p);
            }
            if perms.is_empty() {
                return Err(format!(
                    "Role '{}' must declare at least one permission",
                    state_role.name
                )
                .into());
            }
            let mask = gradient_types::permissions::mask_with_view_project_permission(
                gradient_types::permissions::mask_from(&perms),
            );

            let existing = role::Entity::find()
                .filter(role::Column::Name.eq(&state_role.name))
                .filter(role::Column::Project.eq(project_id))
                .one(self.db)
                .await?;

            if let Some(existing) = existing {
                let mut active: role::ActiveModel = existing.into();
                active.permission = Set(mask);
                active.managed = Set(true);
                active.update(self.db).await?;
                tracing::info!(name = %state_role.name, "Updated managed role");
            } else {
                let active = role::Model {
                    id: RoleId::now_v7(),
                    name: state_role.name.clone(),
                    project: Some(project_id),
                    permission: mask,
                    managed: true,
                }
                .into_active_model();

                active.insert(self.db).await?;
                tracing::info!(name = %state_role.name, "Created managed role");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use gradient_types::permissions::{Permission, mask_grants};
    use std::path::Path;

    #[tokio::test]
    async fn a_declared_role_always_views_its_project() {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let project = rows::project()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let app = StateApplicator {
            db: &connection,
            crypt_secret_file: "",
            credentials_dir: Path::new(""),
            email_enabled: false,
        };

        app.apply_roles(&HashMap::from([(
            "deployer".to_string(),
            StateRole {
                name: "deployer".into(),
                project: project.name.clone(),
                permissions: vec!["triggerEvaluation".into()],
            },
        )]))
        .await
        .unwrap();

        let role = db
            .rows::<role::Entity>()
            .await
            .into_iter()
            .find(|role| role.name == "deployer")
            .expect("the declared role is written");
        assert!(mask_grants(role.permission, Permission::ViewProject));
        assert!(mask_grants(role.permission, Permission::TriggerEvaluation));
        db.finish().await;
    }
}
