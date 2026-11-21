/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{
    load_git_host_connections_usable_by_project_by_name, lookup_id, read_credential,
};
use crate::config::*;
use anyhow::{Context, Result};
use gradient_ci::actions::encrypt_secret_with_file;
use gradient_db::git_host_connection_repository_grants::{
    StateFileRepositoryGrant, check_git_host_connection_task_repository_access_without_grants,
    make_state_file_repository_grants_match_state_file,
};
use gradient_db::lookup::by_id;
use gradient_entity::*;
use gradient_types::triggers::{TriggerConfig, TriggerConfigError, TriggerType};
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, Iterable,
    QueryFilter, Set,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_tasks(
        &self,
        state_tasks: &HashMap<String, StateTask>,
    ) -> Result<(), DynError> {
        let state_users = self.load_managed_accounts_by_lowercase_user_name().await?;
        let project_map = self.project_lookup().await?;
        let mut applied = Vec::with_capacity(state_tasks.len());
        let mut connections_by_project: HashMap<ProjectId, HashMap<String, MGitHostConnection>> =
            HashMap::new();
        let mut projects: HashMap<ProjectId, MProject> = HashMap::new();
        let mut implied_grants: HashSet<StateFileRepositoryGrant> = HashSet::new();

        for state_task in state_tasks.values() {
            let project_id = lookup_id(&project_map, &state_task.project, "Project")?;
            let project = match projects.entry(project_id) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                    by_id::<project::Entity, _>(self.db, project_id)
                        .await?
                        .ok_or_else(|| format!("Project '{}' not found", state_task.project))?,
                ),
            };

            let connections = match connections_by_project.entry(project_id) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                    load_git_host_connections_usable_by_project_by_name(self.db, project_id)
                        .await?,
                ),
            };
            let connection = state_task
                .git_host_connection
                .as_deref()
                .map(|name| {
                    connections.get(name).ok_or_else(|| {
                        format!(
                            "Task '{}': the connection '{}' to the Git host isn't available to project '{}'",
                            state_task.name, name, state_task.project
                        )
                    })
                })
                .transpose()?;
            check_task_git_host_connection_capabilities(state_task, connection)
                .map_err(|e| format!("Task '{}': {}", state_task.name, e))?;
            implied_grants.extend(repository_grants_needed_by_state_file_task(
                state_task,
                project,
                connection,
                connections,
            )?);
            let connection_id = connection.map(|connection| connection.id);

            let existing_task = task::Entity::find()
                .filter(task::Column::Name.eq(&state_task.name))
                .filter(task::Column::Project.eq(project_id))
                .one(self.db)
                .await?;
            let created_by_id = state_users.resolve_creator_username_to_state_account_id(
                &state_task.created_by,
                &format!("task '{}'", state_task.name),
                existing_task.as_ref().map(|task| task.created_by),
            )?;

            let now = now();

            let created = existing_task.is_none();
            let task_row = if let Some(existing) = existing_task {
                let task_id = existing.id;
                let mut proj: task::ActiveModel = existing.into();
                proj.project = Set(project_id);
                proj.active = Set(state_task.active);
                proj.display_name = Set(state_task.display_name.clone());
                proj.description = Set(state_task.description.clone().unwrap_or_default());
                proj.repository = Set(state_task.repository.clone());
                proj.wildcard = Set(state_task.wildcard.clone());
                proj.keep_evaluations = Set(state_task.keep_evaluations);
                proj.created_by = Set(created_by_id);
                proj.concurrency = Set(state_task.concurrency);
                proj.wait_for_workers = Set(state_task.wait_for_workers);
                proj.retry_failed_builds = Set(state_task.retry_failed_builds);
                proj.multi_node_evaluation = Set(state_task.multi_node_evaluation);
                proj.exec_enabled = Set(state_task.exec_enabled);
                proj.only_evaluate = Set(state_task.only_evaluate);
                proj.restrict_evaluation_fetches = Set(state_task.restrict_evaluation_fetches);
                proj.extra_allowed_uri_prefixes =
                    Set(state_task.extra_allowed_uri_prefixes.clone());
                proj.git_host_connection = Set(connection_id);
                proj.report_status = Set(state_task.report_status);
                proj.managed = Set(true);
                proj.update(self.db).await?;
                tracing::info!(name = %state_task.name, "Updated managed task");
                by_id::<task::Entity, _>(self.db, task_id)
                    .await?
                    .ok_or_else(|| format!("Task '{}' vanished after update", state_task.name))?
            } else {
                let proj = task::Model {
                    id: TaskId::now_v7(),
                    project: project_id,
                    name: state_task.name.clone(),
                    active: state_task.active,
                    display_name: state_task.display_name.clone(),
                    description: state_task.description.clone().unwrap_or_default(),
                    repository: state_task.repository.clone(),
                    wildcard: state_task.wildcard.clone(),
                    created_by: created_by_id,
                    last_check_at: now,
                    created_at: now,
                    managed: true,
                    keep_evaluations: state_task.keep_evaluations,
                    concurrency: state_task.concurrency,
                    wait_for_workers: state_task.wait_for_workers,
                    retry_failed_builds: state_task.retry_failed_builds,
                    multi_node_evaluation: state_task.multi_node_evaluation,
                    exec_enabled: state_task.exec_enabled,
                    only_evaluate: state_task.only_evaluate,
                    restrict_evaluation_fetches: state_task.restrict_evaluation_fetches,
                    extra_allowed_uri_prefixes: state_task.extra_allowed_uri_prefixes.clone(),
                    git_host_connection: connection_id,
                    report_status: state_task.report_status,
                    ..Default::default()
                }
                .into_active_model();

                let inserted = proj.insert(self.db).await?;
                tracing::info!(name = %state_task.name, "Created managed task");
                inserted
            };

            let triggers = match &state_task.triggers {
                Some(triggers) => Some(triggers.clone()),
                None if created => Some(vec![StateTrigger::default_polling()]),
                None => None,
            };
            if let Some(triggers) = &triggers {
                self.apply_task_triggers(&state_task.project, &task_row, triggers)
                    .await
                    .map_err(|e| {
                        format!(
                            "Failed to apply triggers for task '{}': {}",
                            state_task.name, e
                        )
                    })?;
            }

            self.apply_flake_input_overrides(task_row.id, &state_task.flake_input_overrides)
                .await
                .map_err(|e| {
                    format!(
                        "Failed to apply flake input overrides for task '{}': {}",
                        state_task.name, e
                    )
                })?;

            applied.push((task_row.id, created_by_id, project_id, state_task));
        }

        for (task_id, created_by_id, project_id, state_task) in applied {
            self.apply_task_actions(
                task_id,
                created_by_id,
                project_id,
                &connections_by_project[&project_id],
                state_task,
            )
            .await
            .map_err(|e| {
                format!(
                    "Failed to apply actions for task '{}': {}",
                    state_task.name, e
                )
            })?;
        }

        make_state_file_repository_grants_match_state_file(self.db, &implied_grants).await?;
        Ok(())
    }

    pub(crate) async fn apply_task_actions(
        &self,
        task_id: TaskId,
        created_by: UserId,
        project_id: ProjectId,
        connections: &HashMap<String, MGitHostConnection>,
        state_task: &StateTask,
    ) -> Result<(), DynError> {
        let task_name = state_task.name.as_str();
        let desired = &state_task.actions;
        let tasks = task_ids_by_name(self.db, project_id).await?;
        let crypt_key = load_secret_bytes(self.crypt_secret_file)
            .map_err(|e| format!("Failed to load crypt secret: {}", e))?;

        let existing = ETaskAction::find()
            .filter(CTaskAction::Task.eq(task_id))
            .all(self.db)
            .await?;
        let existing_by_name: HashMap<String, MTaskAction> =
            existing.into_iter().map(|r| (r.name.clone(), r)).collect();

        let now = now();
        let mut declared: HashSet<String> = HashSet::new();

        for action in desired {
            if !declared.insert(action.name.clone()) {
                return Err(format!(
                    "duplicate action name '{}' in task '{}'",
                    action.name, task_name
                )
                .into());
            }

            let cfg = build_action_config(
                action,
                &ProjectAndTaskNames {
                    project: &state_task.project,
                    task: task_name,
                },
                connections,
                &tasks,
                &ActionConfigCreationSettings {
                    email_enabled: self.email_enabled,
                    crypt_key: crypt_key.expose(),
                    credentials_dir: self.credentials_dir,
                    user_url_destination_allowlist: self.user_url_destination_allowlist,
                },
            )?;
            let cfg_json =
                serde_json::to_value(&cfg).map_err(|e| format!("encoding action config: {e}"))?;
            let events_json = serde_json::to_value(&action.events)
                .map_err(|e| format!("encoding action events: {e}"))?;
            let action_type = cfg.action_type();

            match existing_by_name.get(&action.name) {
                Some(row) => {
                    let mut am: ATaskAction = row.clone().into();
                    am.action_type = Set(action_type);
                    am.config = Set(cfg_json);
                    am.events = Set(events_json);
                    am.active = Set(action.active);
                    am.updated_at = Set(now);
                    am.update(self.db).await?;
                    tracing::info!(
                        task = %task_name,
                        action = %action.name,
                        "Updated task action"
                    );
                }
                None => {
                    let am = MTaskAction {
                        id: TaskActionId::now_v7(),
                        task: task_id,
                        name: action.name.clone(),
                        action_type,
                        config: cfg_json,
                        events: events_json,
                        active: action.active,
                        created_by,
                        created_at: now,
                        updated_at: now,
                        ..Default::default()
                    }
                    .into_active_model();

                    am.insert(self.db).await?;
                    tracing::info!(
                        task = %task_name,
                        action = %action.name,
                        "Created task action"
                    );
                }
            }
        }

        for (name, row) in &existing_by_name {
            if declared.contains(name) {
                continue;
            }
            ETaskAction::delete_by_id(row.id).exec(self.db).await?;
            tracing::info!(
                task = %task_name,
                action = %name,
                "Deleted task action no longer declared in state"
            );
        }

        Ok(())
    }

    pub(crate) async fn apply_flake_input_overrides(
        &self,
        task_id: TaskId,
        desired: &HashMap<String, StateFlakeInputOverride>,
    ) -> Result<(), DynError> {
        use gradient_entity::task_flake_input_override as pfio;

        for (name, o) in desired {
            if o.url.is_some() == o.keep_url {
                return Err(format!(
                    "flake input override '{name}' must set exactly one of `url` or `keep_url`",
                )
                .into());
            }
        }

        let existing = pfio::Entity::find()
            .filter(pfio::Column::Task.eq(task_id))
            .all(self.db)
            .await?;

        let existing_map: HashMap<String, pfio::Model> = existing
            .into_iter()
            .map(|r| (r.input_name.clone(), r))
            .collect();

        let now = chrono::Utc::now().naive_utc();

        for (name, o) in desired {
            let desired_url: Option<String> = if o.keep_url { None } else { o.url.clone() };
            match existing_map.get(name) {
                None => {
                    pfio::Model {
                        id: gradient_entity::ids::FlakeInputOverrideId::now_v7(),
                        task: task_id,
                        input_name: name.clone(),
                        url: desired_url,
                        created_at: now,
                        updated_at: now,
                    }
                    .into_active_model()
                    .insert(self.db)
                    .await?;
                }
                Some(row) if row.url != desired_url => {
                    let mut am: pfio::ActiveModel = row.clone().into();
                    am.url = Set(desired_url);
                    am.updated_at = Set(now);
                    am.update(self.db).await?;
                }
                Some(_) => {}
            }
        }

        for (name, row) in &existing_map {
            if !desired.contains_key(name) {
                let am: pfio::ActiveModel = row.clone().into();
                am.delete(self.db).await?;
            }
        }

        Ok(())
    }
}

struct PreparedTriggerDeclaration {
    config: TriggerConfig,
    key: String,
    id: Option<TaskTriggerId>,
    active: bool,
    encrypted_secret: Option<String>,
}

struct StoredTriggerRowsToKeepAndDelete {
    rows_to_keep_or_insert_in_declared_trigger_order: Vec<Option<MTaskTrigger>>,
    unmatched_trigger_rows_to_delete: Vec<MTaskTrigger>,
}

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_task_triggers(
        &self,
        project: &str,
        task: &MTask,
        desired: &[StateTrigger],
    ) -> Result<(), DynError> {
        if desired.is_empty() {
            return Err(format!("task '{}' must have at least one trigger", task.name).into());
        }

        let declared = desired
            .iter()
            .enumerate()
            .map(|(index, trigger)| {
                self.prepare_trigger_declaration_at_index(project, &task.name, index, trigger)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.ensure_pinned_trigger_belongs_to_task(task, &declared)
            .await?;
        let stored = ETaskTrigger::find()
            .filter(CTaskTrigger::Task.eq(task.id))
            .all(self.db)
            .await?;
        let matched = match_declared_task_triggers_to_stored_rows(&task.name, &declared, stored)?;

        for row in matched.unmatched_trigger_rows_to_delete {
            ETaskTrigger::delete_by_id(row.id).exec(self.db).await?;
        }
        let mut applied = Vec::with_capacity(declared.len());
        for (declaration, row) in declared
            .iter()
            .zip(matched.rows_to_keep_or_insert_in_declared_trigger_order)
        {
            applied.push(match row {
                Some(row) => {
                    update_stored_trigger_fields_if_changed(self.db, row, declaration).await?
                }
                None => {
                    insert_declared_trigger_row_using_pinned_id(self.db, task.id, declaration)
                        .await?
                }
            });
        }

        let default_index = desired.iter().position(|t| t.is_default).unwrap_or(0);
        let default_trigger = applied.swap_remove(default_index);
        if !default_trigger.is_default {
            gradient_db::triggers::make_default(self.db, default_trigger).await?;
        }

        Ok(())
    }

    fn prepare_trigger_declaration_at_index(
        &self,
        project: &str,
        task: &str,
        index: usize,
        trigger: &StateTrigger,
    ) -> Result<PreparedTriggerDeclaration, DynError> {
        let config = build_trigger_config(trigger)?;
        let id = trigger
            .id
            .as_deref()
            .map(|id| {
                id.parse::<TaskTriggerId>().map_err(|e| {
                    format!(
                        "Invalid ID '{}' for trigger {} in task '{}': {}",
                        id, index, task, e
                    )
                })
            })
            .transpose()?;
        let encrypted_secret = match config {
            TriggerConfig::PostRequest { .. } => Some(
                self.read_and_encrypt_post_request_trigger_secret_credential_file(
                    project, task, index,
                )?,
            ),
            _ => None,
        };
        Ok(PreparedTriggerDeclaration {
            key: trigger_key(&config),
            config,
            id,
            active: trigger.active,
            encrypted_secret,
        })
    }

    fn read_and_encrypt_post_request_trigger_secret_credential_file(
        &self,
        project: &str,
        task: &str,
        index: usize,
    ) -> Result<String, DynError> {
        let (contents, path) = read_credential(
            self.credentials_dir,
            "trigger",
            &format!("{project}_{task}_{index}"),
            "secret",
            "Secret file for triggers",
        )?;
        let secret = contents.trim();
        if secret.is_empty() {
            return Err(format!("The secret file {} for the trigger is empty.", path).into());
        }
        encrypt_secret_with_file(self.crypt_secret_file, secret).map_err(|e| {
            format!(
                "Failed to encrypt secret for trigger {} in task \"{}\": {}",
                index, task, e
            )
            .into()
        })
    }

    async fn ensure_pinned_trigger_belongs_to_task(
        &self,
        task: &MTask,
        declared: &[PreparedTriggerDeclaration],
    ) -> Result<(), DynError> {
        let pinned: Vec<TaskTriggerId> = declared.iter().filter_map(|d| d.id).collect();
        if pinned.is_empty() {
            return Ok(());
        }
        let Some(foreign) = ETaskTrigger::find()
            .filter(CTaskTrigger::Id.is_in(pinned))
            .filter(CTaskTrigger::Task.ne(task.id))
            .one(self.db)
            .await?
        else {
            return Ok(());
        };
        let other_task = by_id::<task::Entity, _>(self.db, foreign.task)
            .await?
            .map(|other| other.name)
            .unwrap_or_default();
        let index = declared
            .iter()
            .position(|d| d.id == Some(foreign.id))
            .unwrap_or_default();
        Err(format!(
            "Triggers in task \"{}\" already use the pinned ID {} of trigger {} in task \"{}\".",
            other_task, foreign.id, index, task.name
        )
        .into())
    }
}

fn match_declared_task_triggers_to_stored_rows(
    task: &str,
    declared: &[PreparedTriggerDeclaration],
    stored: Vec<MTaskTrigger>,
) -> Result<StoredTriggerRowsToKeepAndDelete, DynError> {
    let mut pool = stored
        .into_iter()
        .map(|row| {
            let config = TriggerConfig::parse_row(row.trigger_type, &row.config)
                .context("parse existing trigger")?;
            Ok((trigger_key(&config), row))
        })
        .collect::<Result<Vec<_>, DynError>>()?;

    let mut matched: Vec<Option<MTaskTrigger>> = declared
        .iter()
        .map(|d| {
            d.id.and_then(|id| {
                remove_first_matching_stored_trigger_row(&mut pool, |(_, row)| row.id == id)
            })
        })
        .collect();

    for (index, (declaration, row)) in declared.iter().zip(&matched).enumerate() {
        let (Some(declared_id), None) = (declaration.id, row) else {
            continue;
        };
        if let Some((_, existing)) = pool.iter().find(|(key, _)| *key == declaration.key) {
            return Err(format!(
                "The existing row for trigger {} of task '{}' and the state use different IDs: ID {} in the row, ID {} in the state. IDs are immutable.",
                index, task, existing.id, declared_id
            )
            .into());
        }
    }

    for (declaration, row) in declared.iter().zip(matched.iter_mut()) {
        if declaration.id.is_none() {
            *row = remove_first_matching_stored_trigger_row(&mut pool, |(key, _)| {
                *key == declaration.key
            });
        }
    }

    Ok(StoredTriggerRowsToKeepAndDelete {
        rows_to_keep_or_insert_in_declared_trigger_order: matched,
        unmatched_trigger_rows_to_delete: pool.into_iter().map(|(_, row)| row).collect(),
    })
}

fn remove_first_matching_stored_trigger_row(
    pool: &mut Vec<(String, MTaskTrigger)>,
    matches: impl Fn(&(String, MTaskTrigger)) -> bool,
) -> Option<MTaskTrigger> {
    let position = pool.iter().position(matches)?;
    Some(pool.swap_remove(position).1)
}

async fn update_stored_trigger_fields_if_changed<C: ConnectionTrait>(
    db: &C,
    row: MTaskTrigger,
    declaration: &PreparedTriggerDeclaration,
) -> Result<MTaskTrigger, sea_orm::DbErr> {
    let mut active = row.clone().into_active_model();
    active
        .trigger_type
        .set_if_not_equals(declaration.config.trigger_type());
    active
        .config
        .set_if_not_equals(declaration.config.to_db_json());
    active.active.set_if_not_equals(declaration.active);
    active
        .encrypted_secret
        .set_if_not_equals(declaration.encrypted_secret.clone());
    if !active.is_changed() {
        return Ok(row);
    }
    active.updated_at = Set(gradient_types::now());
    active.update(db).await
}

async fn insert_declared_trigger_row_using_pinned_id<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    declaration: &PreparedTriggerDeclaration,
) -> Result<MTaskTrigger, sea_orm::DbErr> {
    let now = gradient_types::now();
    MTaskTrigger {
        id: declaration.id.unwrap_or_else(TaskTriggerId::now_v7),
        task,
        trigger_type: declaration.config.trigger_type(),
        config: declaration.config.to_db_json(),
        active: declaration.active,
        encrypted_secret: declaration.encrypted_secret.clone(),
        created_at: now,
        updated_at: now,
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
}

const DEFAULT_POLLING_TRIGGER_INTERVAL_SECS: u32 = 300;

pub(crate) fn build_trigger_config(t: &StateTrigger) -> Result<TriggerConfig, TriggerConfigError> {
    let mut config = t.config.as_object().cloned().unwrap_or_default();
    if t.trigger_type == TriggerType::Polling {
        config
            .entry("interval_secs")
            .or_insert(DEFAULT_POLLING_TRIGGER_INTERVAL_SECS.into());
    }
    let config = TriggerConfig::parse_row(t.trigger_type, &serde_json::Value::Object(config))?;
    config.validate_path_filter_pattern_limits()?;
    Ok(config)
}

pub(crate) fn trigger_key(cfg: &TriggerConfig) -> String {
    let json = cfg.to_db_json();
    let canonical = serde_json::to_string(&json).unwrap_or_default();
    format!("{}|{}", i16::from(cfg.trigger_type()), canonical)
}

pub(crate) struct ActionConfigCreationSettings<'a> {
    pub(crate) email_enabled: bool,
    pub(crate) crypt_key: &'a [u8],
    pub(crate) credentials_dir: &'a Path,
    pub(crate) user_url_destination_allowlist:
        &'a gradient_util::http_validation::UserUrlDestinationAllowlist,
}

pub(crate) struct ProjectAndTaskNames<'a> {
    pub(crate) project: &'a str,
    pub(crate) task: &'a str,
}

fn repository_grants_needed_by_state_file_task(
    state_task: &StateTask,
    project: &MProject,
    task_connection: Option<&MGitHostConnection>,
    connections: &HashMap<String, MGitHostConnection>,
) -> Result<Vec<StateFileRepositoryGrant>, String> {
    let open_pr_connections = state_task
        .actions
        .iter()
        .filter(|action| action.action_type == "open_pr")
        .filter_map(|action| action.config.get("git_host_connection")?.as_str())
        .filter_map(|name| connections.get(name));
    let mut implied = Vec::new();
    for connection in task_connection.into_iter().chain(open_pr_connections) {
        let fitting = check_git_host_connection_task_repository_access_without_grants(
            connection,
            project,
            &state_task.repository,
        )
        .map_err(|refusal| {
            format!(
                "Task '{}': connection '{}' for the Git host: {}",
                state_task.name, connection.name, refusal
            )
        })?;
        if fitting.needs_grant {
            implied.push(StateFileRepositoryGrant::new(
                connection.id,
                project.id,
                &fitting.repository,
            ));
        }
    }
    Ok(implied)
}

/// Action secrets come from credentials `gradient_action_${project}_${task}_${name}_${field}`, so actions with the same name in different tasks never share secrets. They go through encryption with the crypt key of the server before storage, in line with the REST `create_action` path.
pub(crate) fn build_action_config(
    a: &StateAction,
    owner: &ProjectAndTaskNames,
    connections: &HashMap<String, MGitHostConnection>,
    tasks: &HashMap<String, TaskId>,
    settings: &ActionConfigCreationSettings,
) -> Result<ActionConfig, DynError> {
    let ActionConfigCreationSettings {
        email_enabled,
        crypt_key,
        credentials_dir,
        user_url_destination_allowlist,
    } = *settings;
    let want = |k: &str| -> Result<&serde_json::Value, DynError> {
        a.config
            .get(k)
            .ok_or_else(|| format!("action '{}' config missing '{}'", a.name, k).into())
    };
    let encrypt = |plain: &str| -> Result<String, DynError> {
        gradient_ci::actions::encrypt_action_secret(plain, crypt_key)
            .map_err(|e| format!("encrypt action secret: {e}").into())
    };

    match a.action_type.as_str() {
        "send_mail" => {
            if !email_enabled {
                return Err(format!(
                    "action '{}' in task '{}' is type 'send_mail' but SMTP is not enabled on this server",
                    a.name, owner.task
                )
                .into());
            }
            let recipients: Vec<String> = want("recipients")?
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .ok_or_else(|| format!("action '{}': recipients must be a list", a.name))?;
            if recipients.is_empty() {
                return Err(format!("action '{}': recipients must be non-empty", a.name).into());
            }
            let subject_template = a
                .config
                .get("subject_template")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            Ok(ActionConfig::SendMail {
                recipients,
                subject_template,
            })
        }
        "send_web_request" => {
            let url = string_field(a, "url")?;
            checked_url(&a.name, &url, user_url_destination_allowlist)?;
            let token = action_credential(credentials_dir, owner, a, "token")?
                .map(|plain| encrypt(&plain))
                .transpose()?;
            Ok(ActionConfig::SendWebRequest { url, token })
        }
        "send_matrix_message" => {
            let homeserver = string_field(a, "homeserver")?;
            checked_url(&a.name, &homeserver, user_url_destination_allowlist)?;
            let room_id = string_field(a, "room_id")?;
            if !gradient_types::actions::is_matrix_room_id(&room_id) {
                return Err(format!(
                    "action '{}': room_id must be a Matrix room ID like !abc123 or !abc:example.org",
                    a.name
                )
                .into());
            }

            let plain = action_credential(credentials_dir, owner, a, "access_token")?
                .ok_or_else(|| format!("action '{}': access_token_file is required", a.name))?;
            Ok(ActionConfig::SendMatrixMessage {
                homeserver,
                room_id,
                access_token: Some(encrypt(&plain)?),
            })
        }
        "send_slack_message" => {
            let plain = action_credential(credentials_dir, owner, a, "webhook_url")?
                .ok_or_else(|| format!("action '{}': webhook_url_file is required", a.name))?;
            checked_url(&a.name, &plain, user_url_destination_allowlist)?;
            Ok(ActionConfig::SendSlackMessage {
                webhook_url: Some(encrypt(&plain)?),
            })
        }
        "open_pr" => {
            let connection_name = want("git_host_connection")?.as_str().ok_or_else(|| {
                format!(
                    "action '{}': git_host_connection needs to be a string",
                    a.name
                )
            })?;
            let git_host_connection_id = connections.get(connection_name).ok_or_else(|| {
                format!(
                    "action '{}': connection to git host '{}' is not available in the task's project",
                    a.name, connection_name
                )
            })?
                .id;
            let generator: PatchGeneratorKind = parse_action_enum(a, "generator")?;
            let granularity: PrGranularity = parse_action_enum(a, "granularity")?;
            let verify_gate: VerifyGate = parse_action_enum(a, "verify_gate")?;
            let branch_pattern = a
                .config
                .get("branch_pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("gradient/flake-lock-update")
                .to_owned();
            let title_template = a
                .config
                .get("title_template")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let body_template = a
                .config
                .get("body_template")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let update_existing = a
                .config
                .get("update_existing")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            Ok(ActionConfig::OpenPr {
                git_host_connection_id,
                generator,
                granularity,
                verify_gate,
                branch_pattern,
                title_template,
                body_template,
                update_existing,
            })
        }
        "start_evaluation" => {
            let target = string_field(a, "task")?;
            let task = *tasks.get(&target).ok_or_else(|| {
                format!(
                    "action '{}': task '{}' not found in the task's project",
                    a.name, target
                )
            })?;
            Ok(ActionConfig::StartEvaluation { task })
        }
        other => Err(format!(
            "action '{}' has invalid type '{}': expected {}",
            a.name,
            other,
            action_type_names()
        )
        .into()),
    }
}

pub(crate) fn check_task_git_host_connection_capabilities(
    task: &StateTask,
    connection: Option<&MGitHostConnection>,
) -> Result<(), String> {
    if task.report_status {
        match connection {
            None => {
                return Err("A task with report_status needs a git_host_connection field.".into());
            }
            Some(connection) if !connection.reports_status => {
                return Err(format!(
                    "report_status needs a connection to a Git host with reports_status set. The connection '{}' has reports_status = false.",
                    connection.name
                ));
            }
            Some(_) => {}
        }
    }
    let has_reporter_trigger = task.triggers.iter().flatten().any(|trigger| {
        matches!(
            trigger.trigger_type,
            TriggerType::ReporterPush | TriggerType::ReporterPullRequest
        )
    });
    if has_reporter_trigger {
        match connection {
            None => return Err("The `reporter_push` and `reporter_pull_request` triggers require the task's `git_host_connection`.".into()),
            Some(connection) if !connection.receives_events => {
                return Err(format!(
                    "The reporter_push/reporter_pull_request triggers need a connection to a git host with receives_events set. '{}' does not have that setting.",
                    connection.name
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

async fn task_ids_by_name<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<HashMap<String, TaskId>, DynError> {
    Ok(task::Entity::find()
        .filter(task::Column::Project.eq(project))
        .all(db)
        .await?
        .into_iter()
        .map(|task| (task.name, task.id))
        .collect())
}

fn action_type_names() -> String {
    ActionType::iter()
        .map(ActionType::as_str)
        .collect::<Vec<_>>()
        .join("/")
}

fn string_field(a: &StateAction, key: &str) -> Result<String, DynError> {
    a.config
        .get(key)
        .ok_or_else(|| format!("action '{}' config missing '{}'", a.name, key))?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("action '{}': {} must be a string", a.name, key).into())
}

fn checked_url(
    action: &str,
    url: &str,
    user_url_destination_allowlist: &gradient_util::http_validation::UserUrlDestinationAllowlist,
) -> Result<(), DynError> {
    gradient_util::http_validation::parse_and_check_allowed_outbound_url(
        url,
        user_url_destination_allowlist,
    )
    .map(drop)
    .map_err(|e| format!("action '{action}': {e}").into())
}

fn action_credential(
    credentials_dir: &Path,
    owner: &ProjectAndTaskNames,
    a: &StateAction,
    field: &str,
) -> Result<Option<String>, DynError> {
    if a.config.get(format!("{field}_file")).is_none() {
        return Ok(None);
    }

    let (plain, _) = read_credential(
        credentials_dir,
        "action",
        &format!("{}_{}_{}", owner.project, owner.task, a.name),
        field,
        &format!("action {field} file"),
    )?;
    Ok(Some(plain.trim().to_owned()))
}

fn parse_action_enum<T>(a: &StateAction, key: &str) -> Result<T, DynError>
where
    T: Default + serde::de::DeserializeOwned,
{
    match a.config.get(key) {
        None | Some(serde_json::Value::Null) => Ok(T::default()),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("action '{}': invalid '{}': {}", a.name, key, e).into()),
    }
}

#[cfg(test)]
mod state_file_task_repository_grant_tests;

#[cfg(test)]
mod trigger_helper_tests {
    use super::{build_trigger_config, trigger_key};
    use crate::StateTrigger;
    use gradient_types::triggers::{ChangedFilePathFilter, TriggerConfig, TriggerType};

    pub(crate) fn polling_trigger(interval_secs: u64) -> StateTrigger {
        StateTrigger {
            trigger_type: TriggerType::Polling,
            config: serde_json::json!({ "interval_secs": interval_secs }),
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        }
    }

    #[test]
    fn build_polling_trigger() {
        let t = polling_trigger(60);
        let cfg = build_trigger_config(&t).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::Polling {
                interval_secs: 60,
                branch: None,
                path_filter: ChangedFilePathFilter::default(),
            }
        );
    }

    #[test]
    fn build_polling_defaults_interval_when_missing() {
        let t = StateTrigger {
            trigger_type: TriggerType::Polling,
            config: serde_json::Value::Null,
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        };
        let cfg = build_trigger_config(&t).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::Polling {
                interval_secs: 300,
                branch: None,
                path_filter: ChangedFilePathFilter::default(),
            }
        );
    }

    #[test]
    fn build_polling_rejects_too_small_interval() {
        let t = polling_trigger(5);
        let err = build_trigger_config(&t).unwrap_err();
        let full = format!("{err:#}");
        assert!(
            full.contains("interval_secs") || full.contains("validation"),
            "expected polling interval rejection, got: {full}"
        );
    }

    #[test]
    fn build_trigger_config_rejects_too_long_path_filter_pattern() {
        let mut t = polling_trigger(60);
        t.config["include_paths"] = serde_json::json!(["a".repeat(513)]);
        let err = build_trigger_config(&t).unwrap_err();
        assert!(
            err.to_string().contains("512 characters"),
            "expected path filter pattern length rejection, got: {err}"
        );
    }

    #[test]
    fn build_time_trigger() {
        let t = StateTrigger {
            trigger_type: TriggerType::Time,
            config: serde_json::json!({ "cron": "0 0 2 * * *" }),
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        };
        let cfg = build_trigger_config(&t).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::Time {
                cron: "0 0 2 * * *".into(),
                branch: None,
            }
        );
    }

    #[test]
    fn build_time_trigger_with_branch_and_five_field_cron() {
        let t = StateTrigger {
            trigger_type: TriggerType::Time,
            config: serde_json::json!({ "cron": "0 2 * * *", "branch": "release" }),
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        };
        let cfg = build_trigger_config(&t).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::Time {
                cron: "0 0 2 * * *".into(),
                branch: Some("release".into()),
            }
        );
    }

    #[test]
    fn build_time_trigger_requires_cron() {
        let t = StateTrigger {
            trigger_type: TriggerType::Time,
            config: serde_json::json!({}),
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        };
        let err = build_trigger_config(&t).unwrap_err();
        assert!(err.to_string().contains("cron"));
    }

    #[test]
    fn build_reporter_push_trigger() {
        let t = StateTrigger {
            trigger_type: TriggerType::ReporterPush,
            config: serde_json::json!({
                "branches": ["main"],
                "tags": [],
                "releases_only": false,
                "include_flake_inputs": true,
            }),
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        };
        let cfg = build_trigger_config(&t).unwrap();
        assert_eq!(
            cfg,
            TriggerConfig::ReporterPush {
                branches: vec!["main".into()],
                tags: vec![],
                releases_only: false,
                include_flake_inputs: true,
                path_filter: ChangedFilePathFilter::default(),
            }
        );
    }

    #[test]
    fn reporter_push_trigger_include_paths_and_exclude_paths_round_trip_through_database_and_export()
     {
        let declared = StateTrigger {
            trigger_type: TriggerType::ReporterPush,
            config: serde_json::json!({
                "branches": ["main"],
                "include_paths": ["src/*"],
                "exclude_paths": ["src/*.md"],
            }),
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        };
        let cfg = build_trigger_config(&declared).unwrap();
        let stored = gradient_entity::task_trigger::Model {
            trigger_type: cfg.trigger_type(),
            config: cfg.to_db_json(),
            active: true,
            ..Default::default()
        };
        assert_eq!(stored.config["include_paths"], serde_json::json!(["src/*"]));
        assert_eq!(
            stored.config["exclude_paths"],
            serde_json::json!(["src/*.md"])
        );

        let exported = crate::export::export_trigger(&stored).unwrap();
        assert_eq!(build_trigger_config(&exported).unwrap(), cfg);
    }

    #[test]
    fn trigger_key_differs_by_type() {
        let polling = TriggerConfig::Polling {
            interval_secs: 60,
            branch: None,
            path_filter: ChangedFilePathFilter::default(),
        };
        let time = TriggerConfig::Time {
            cron: "0 0 * * * *".into(),
            branch: None,
        };
        assert_ne!(trigger_key(&polling), trigger_key(&time));
    }
}

#[cfg(test)]
mod action_helper_tests {
    use super::*;
    use uuid::Uuid;

    pub(crate) fn key() -> Vec<u8> {
        b"01234567890123456789012345678901".to_vec()
    }

    #[test]
    fn build_send_mail_round_trip() {
        let a = StateAction {
            name: "ops".into(),
            action_type: "send_mail".into(),
            active: true,
            events: vec!["build.failed".into()],
            config: serde_json::json!({
                "recipients": ["ops@example.com"],
                "subject_template": "[Gradient] {event}",
            }),
        };
        let cfg = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap();
        match cfg {
            ActionConfig::SendMail {
                recipients,
                subject_template,
            } => {
                assert_eq!(recipients, vec!["ops@example.com".to_string()]);
                assert_eq!(subject_template.as_deref(), Some("[Gradient] {event}"));
            }
            other => panic!("expected SendMail, got {other:?}"),
        }
    }

    #[test]
    fn build_send_mail_rejects_when_email_disabled() {
        let a = StateAction {
            name: "ops".into(),
            action_type: "send_mail".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "recipients": ["ops@example.com"] }),
        };
        let err = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: false,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("SMTP"), "got: {err}");
    }

    #[test]
    fn build_send_mail_requires_non_empty_recipients() {
        let a = StateAction {
            name: "ops".into(),
            action_type: "send_mail".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "recipients": [] }),
        };
        let err = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("recipients"), "got: {err}");
    }

    #[test]
    fn tasks_in_different_projects_with_same_action_name_read_own_secret_files() {
        let dir =
            std::env::temp_dir().join(format!("gradient-action-credentials-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("gradient_action_acme_ci_notify_token"),
            "acme-token\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("gradient_action_globex_ci_notify_token"),
            "globex-token",
        )
        .unwrap();
        let a = StateAction {
            name: "notify".into(),
            action_type: "send_web_request".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "url": "https://hooks.example.com/x", "token_file": "/run/x" }),
        };

        let acme = action_credential(
            &dir,
            &ProjectAndTaskNames {
                project: "acme",
                task: "ci",
            },
            &a,
            "token",
        )
        .unwrap();
        let globex = action_credential(
            &dir,
            &ProjectAndTaskNames {
                project: "globex",
                task: "ci",
            },
            &a,
            "token",
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(acme.as_deref(), Some("acme-token"));
        assert_eq!(globex.as_deref(), Some("globex-token"));
    }

    #[test]
    fn build_send_web_request_without_token() {
        let a = StateAction {
            name: "hook".into(),
            action_type: "send_web_request".into(),
            active: true,
            events: vec!["build.completed".into()],
            config: serde_json::json!({ "url": "https://hooks.example.com/x" }),
        };
        let cfg = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap();
        match cfg {
            ActionConfig::SendWebRequest { url, token } => {
                assert_eq!(url, "https://hooks.example.com/x");
                assert!(token.is_none());
            }
            other => panic!("expected SendWebRequest, got {other:?}"),
        }
    }

    fn open_pr(connection: &str) -> StateAction {
        StateAction {
            name: "flake-update".into(),
            action_type: "open_pr".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({ "git_host_connection": connection }),
        }
    }

    fn build_with_connections(
        a: &StateAction,
        connections: &HashMap<String, MGitHostConnection>,
    ) -> Result<ActionConfig, DynError> {
        build_action_config(
            a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            connections,
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
    }

    #[test]
    fn test_open_pr_git_host_connection_name_resolves_to_id() {
        let forge = GitHostConnectionId::new(Uuid::from_u128(3));
        let connections = HashMap::from([(
            "forge".to_string(),
            MGitHostConnection {
                id: forge,
                name: "forge".into(),
                ..Default::default()
            },
        )]);

        let cfg = build_with_connections(&open_pr("forge"), &connections).unwrap();

        assert!(matches!(
            cfg,
            ActionConfig::OpenPr { git_host_connection_id, .. } if git_host_connection_id == forge
        ));
    }

    #[test]
    fn open_pr_with_unavailable_connection_fails_with_error_naming_it() {
        let err = build_with_connections(&open_pr("missing"), &HashMap::new()).unwrap_err();

        assert!(err.to_string().contains("'missing'"), "got: {err}");
    }

    #[test]
    fn build_rejects_unknown_action_type() {
        let a = StateAction {
            name: "x".into(),
            action_type: "garbage".into(),
            active: true,
            events: vec![],
            config: serde_json::json!({}),
        };
        let err = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid type"), "got: {err}");
    }

    #[test]
    fn build_matrix_action_requires_an_access_token_file() {
        let a = StateAction {
            name: "matrix".into(),
            action_type: "send_matrix_message".into(),
            active: true,
            events: vec!["build.failed".into()],
            config: serde_json::json!({
                "homeserver": "https://matrix.example.org",
                "room_id": "!ops:example.org",
            }),
        };
        let err = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("access_token_file"), "got: {err}");
    }

    #[test]
    fn build_matrix_action_rejects_a_room_alias() {
        let a = StateAction {
            name: "matrix".into(),
            action_type: "send_matrix_message".into(),
            active: true,
            events: vec!["build.failed".into()],
            config: serde_json::json!({
                "homeserver": "https://matrix.example.org",
                "room_id": "#ops:example.org",
                "access_token_file": "/run/secrets/t",
            }),
        };
        let err = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("room_id"), "got: {err}");
    }

    #[test]
    fn build_slack_action_requires_a_webhook_url_file() {
        let a = StateAction {
            name: "slack".into(),
            action_type: "send_slack_message".into(),
            active: true,
            events: vec!["build.failed".into()],
            config: serde_json::json!({}),
        };
        let err = build_action_config(
            &a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "web",
            },
            &HashMap::new(),
            &HashMap::new(),
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("webhook_url_file"), "got: {err}");
    }

    #[test]
    fn slack_webhook_rejects_a_loopback_url() {
        let err = checked_url("slack", "http://127.0.0.1/hook", &Default::default()).unwrap_err();
        assert!(err.to_string().contains("private address"), "got: {err}");

        let loopback = gradient_util::http_validation::UserUrlDestinationAllowlist::parse(&[
            "127.0.0.0/8".into(),
        ])
        .unwrap();
        checked_url("slack", "http://127.0.0.1/hook", &loopback).unwrap();
    }

    fn start_evaluation(task: &str) -> StateAction {
        StateAction {
            name: "evaluate-deploy".into(),
            action_type: "start_evaluation".into(),
            active: true,
            events: vec!["exec_job.completed".into()],
            config: serde_json::json!({ "task": task }),
        }
    }

    fn build_with_tasks(
        a: &StateAction,
        tasks: &HashMap<String, TaskId>,
    ) -> Result<ActionConfig, DynError> {
        build_action_config(
            a,
            &ProjectAndTaskNames {
                project: "acme",
                task: "update",
            },
            &HashMap::new(),
            tasks,
            &ActionConfigCreationSettings {
                email_enabled: true,
                crypt_key: &key(),
                credentials_dir: Path::new("/nonexistent"),
                user_url_destination_allowlist: &Default::default(),
            },
        )
    }

    #[test]
    fn start_evaluation_resolves_its_task_by_name() {
        let deploy = TaskId::new(Uuid::from_u128(7));
        let tasks = HashMap::from([("deploy".to_string(), deploy)]);

        let cfg = build_with_tasks(&start_evaluation("deploy"), &tasks).unwrap();

        assert_eq!(cfg, ActionConfig::StartEvaluation { task: deploy });
    }

    #[test]
    fn start_evaluation_names_a_missing_task() {
        let err = build_with_tasks(&start_evaluation("deploy"), &HashMap::new()).unwrap_err();

        assert_eq!(
            err.to_string(),
            "action 'evaluate-deploy': task 'deploy' not found in the task's project"
        );
    }
}

#[cfg(test)]
mod tests_check_task_git_host_connection_capabilities {
    use super::*;

    fn task(report_status: bool, triggers: Vec<StateTrigger>) -> StateTask {
        serde_json::from_value(serde_json::json!({
            "name": "web",
            "project": "acme",
            "display_name": "Web",
            "repository": "https://example.com/acme/web.git",
            "created_by": "alice",
            "git_host_connection": "forge",
            "report_status": report_status,
            "triggers": triggers,
        }))
        .unwrap()
    }

    fn push_trigger() -> StateTrigger {
        StateTrigger {
            trigger_type: TriggerType::ReporterPush,
            config: serde_json::json!({}),
            active: true,
            is_default: false,
            secret_file: None,
            id: None,
        }
    }

    fn connection(receives_events: bool, reports_status: bool) -> MGitHostConnection {
        MGitHostConnection {
            name: "forge".into(),
            receives_events,
            reports_status,
            ..Default::default()
        }
    }

    #[test]
    fn report_status_is_refused_without_connection_or_with_reports_status_false() {
        let reporting = task(true, vec![StateTrigger::default_polling()]);

        let without = check_task_git_host_connection_capabilities(&reporting, None).unwrap_err();
        assert!(without.contains("git_host_connection"), "got: {without}");

        let silent = connection(true, false);
        let err =
            check_task_git_host_connection_capabilities(&reporting, Some(&silent)).unwrap_err();
        assert!(
            err.contains("reports_status") && err.contains("'forge'"),
            "got: {err}"
        );
    }

    #[test]
    fn push_trigger_rejects_connection_without_receives_events_and_accepts_connection_with_both_flags()
     {
        let pushed = task(true, vec![push_trigger()]);

        let deaf = connection(false, true);
        let err = check_task_git_host_connection_capabilities(&pushed, Some(&deaf)).unwrap_err();
        assert!(err.contains("receives_events"), "got: {err}");

        let capable = connection(true, true);
        assert_eq!(
            check_task_git_host_connection_capabilities(&pushed, Some(&capable)),
            Ok(())
        );
    }
}

#[cfg(test)]
mod declared_task_trigger_application_tests {
    use super::trigger_helper_tests::polling_trigger;
    use super::*;
    use gradient_ci::actions::decrypt_secret_with_file;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use sea_orm::DatabaseConnection;
    use tempfile::TempDir;

    struct StateApplyTestTempCredentialFiles {
        dir: TempDir,
        crypt_file: String,
    }

    impl StateApplyTestTempCredentialFiles {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let crypt_file = dir.path().join("crypt");
            std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").unwrap();
            Self {
                crypt_file: crypt_file.to_str().unwrap().to_owned(),
                dir,
            }
        }

        fn write_post_request_trigger_secret_credential_file_for_index(
            &self,
            index: usize,
            contents: &str,
        ) {
            std::fs::write(
                self.dir.path().join(format!(
                    "gradient_trigger_test-project_test-task_{index}_secret"
                )),
                contents,
            )
            .unwrap();
        }

        async fn apply(
            &self,
            db: &DatabaseConnection,
            task: &MTask,
            triggers: &[StateTrigger],
        ) -> Result<(), DynError> {
            let transaction = sea_orm::TransactionTrait::begin(db).await?;
            let app = StateApplicator {
                db: &transaction,
                crypt_secret_file: &self.crypt_file,
                credentials_dir: self.dir.path(),
                email_enabled: false,
                user_url_destination_allowlist: &Default::default(),
            };
            app.apply_task_triggers("test-project", task, triggers)
                .await?;
            transaction.commit().await?;
            Ok(())
        }

        fn decrypt_stored_trigger_secret_with_test_crypt_key_file(
            &self,
            row: &MTaskTrigger,
        ) -> String {
            decrypt_secret_with_file(&self.crypt_file, row.encrypted_secret.as_deref().unwrap())
                .unwrap()
                .expose()
                .to_owned()
        }
    }

    async fn database_with_user_project_and_task() -> TestDatabase {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        rows::project()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        rows::task()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        db
    }

    fn build_post_request_state_trigger_with_branch_and_optional_pinned_id(
        branch: &str,
        id: Option<TaskTriggerId>,
    ) -> StateTrigger {
        StateTrigger {
            trigger_type: TriggerType::PostRequest,
            config: serde_json::json!({ "branch": branch }),
            active: true,
            is_default: false,
            secret_file: Some("/run/secrets/trigger".into()),
            id: id.map(|id| id.to_string()),
        }
    }

    fn pinned_polling_trigger_with_60_second_interval(id: TaskTriggerId) -> StateTrigger {
        StateTrigger {
            id: Some(id.to_string()),
            ..polling_trigger(60)
        }
    }

    #[tokio::test]
    async fn changed_credential_file_updates_kept_post_request_trigger_secret_on_next_state_apply()
    {
        let db = database_with_user_project_and_task().await;
        let files = StateApplyTestTempCredentialFiles::new();
        let trigger =
            build_post_request_state_trigger_with_branch_and_optional_pinned_id("main", None);
        files.write_post_request_trigger_secret_credential_file_for_index(0, "s3cret\n");

        files
            .apply(
                &db.connection(),
                &rows::task(),
                std::slice::from_ref(&trigger),
            )
            .await
            .unwrap();
        let first = db.rows::<ETaskTrigger>().await.remove(0);
        assert_eq!(
            files.decrypt_stored_trigger_secret_with_test_crypt_key_file(&first),
            "s3cret"
        );

        files.write_post_request_trigger_secret_credential_file_for_index(0, "rotated");
        files
            .apply(&db.connection(), &rows::task(), &[trigger])
            .await
            .unwrap();
        let rows = db.rows::<ETaskTrigger>().await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, first.id);
        assert_ne!(rows[0].encrypted_secret, first.encrypted_secret);
        assert_eq!(
            files.decrypt_stored_trigger_secret_with_test_crypt_key_file(&rows[0]),
            "rotated"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn post_request_trigger_with_empty_or_whitespace_secret_credential_file_fails_state_apply_and_stores_no_secret()
     {
        let db = database_with_user_project_and_task().await;
        let files = StateApplyTestTempCredentialFiles::new();
        files.write_post_request_trigger_secret_credential_file_for_index(0, " \n");

        let err = files
            .apply(
                &db.connection(),
                &rows::task(),
                &[
                    build_post_request_state_trigger_with_branch_and_optional_pinned_id(
                        "main", None,
                    ),
                ],
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("gradient_trigger_test-project_test-task_0_secret"),
            "{err}"
        );
        assert!(err.contains("empty"), "{err}");
        assert!(db.rows::<ETaskTrigger>().await.is_empty());
        db.finish().await;
    }

    #[tokio::test]
    async fn state_trigger_with_pinned_id_keeps_id_when_branch_changes() {
        let db = database_with_user_project_and_task().await;
        let files = StateApplyTestTempCredentialFiles::new();
        files.write_post_request_trigger_secret_credential_file_for_index(0, "first");
        files.write_post_request_trigger_secret_credential_file_for_index(1, "second");
        let pinned = TaskTriggerId::now_v7();
        let pinned_trigger = |branch| {
            build_post_request_state_trigger_with_branch_and_optional_pinned_id(
                branch,
                Some(pinned),
            )
        };

        files
            .apply(&db.connection(), &rows::task(), &[pinned_trigger("main")])
            .await
            .unwrap();
        files
            .apply(
                &db.connection(),
                &rows::task(),
                &[pinned_trigger("release")],
            )
            .await
            .unwrap();
        let rows = db.rows::<ETaskTrigger>().await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, pinned);
        assert_eq!(rows[0].config["branch"], "release");

        let twin = TaskTriggerId::now_v7();
        let twins = [
            pinned_trigger("main"),
            build_post_request_state_trigger_with_branch_and_optional_pinned_id("main", Some(twin)),
        ];
        for _ in 0..2 {
            files
                .apply(&db.connection(), &rows::task(), &twins)
                .await
                .unwrap();
        }
        let mut ids: Vec<_> = db
            .rows::<ETaskTrigger>()
            .await
            .iter()
            .map(|row| row.id)
            .collect();
        ids.sort();
        let mut expected = vec![pinned, twin];
        expected.sort();
        assert_eq!(ids, expected);
        db.finish().await;
    }

    #[tokio::test]
    async fn rejects_pinned_trigger_from_other_task() {
        let db = database_with_user_project_and_task().await;
        let other_task = MTask {
            id: TaskId::now_v7(),
            name: "other-task".into(),
            ..rows::task()
        };
        other_task
            .clone()
            .into_active_model()
            .insert(&db.connection())
            .await
            .unwrap();
        let files = StateApplyTestTempCredentialFiles::new();
        let pinned = TaskTriggerId::now_v7();
        files
            .apply(
                &db.connection(),
                &other_task,
                &[pinned_polling_trigger_with_60_second_interval(pinned)],
            )
            .await
            .unwrap();

        let err = files
            .apply(
                &db.connection(),
                &rows::task(),
                &[pinned_polling_trigger_with_60_second_interval(pinned)],
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("\"test-task\"") && err.contains("\"other-task\""),
            "{err}"
        );
        let rows = db.rows::<ETaskTrigger>().await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].task, other_task.id);
        db.finish().await;
    }

    #[tokio::test]
    async fn declared_trigger_id_next_to_unpinned_stored_row_with_same_config_fails_apply_state() {
        let db = database_with_user_project_and_task().await;
        let files = StateApplyTestTempCredentialFiles::new();
        files
            .apply(&db.connection(), &rows::task(), &[polling_trigger(60)])
            .await
            .unwrap();
        let existing = db.rows::<ETaskTrigger>().await.remove(0);

        let declared = TaskTriggerId::now_v7();
        let err = files
            .apply(
                &db.connection(),
                &rows::task(),
                &[pinned_polling_trigger_with_60_second_interval(declared)],
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains(&declared.to_string()), "{err}");
        assert!(err.contains(&existing.id.to_string()), "{err}");
        let rows = db.rows::<ETaskTrigger>().await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].id, existing.id);
        db.finish().await;
    }
}

#[cfg(test)]
mod declared_task_git_host_connection_application_tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::ids::project_id;
    use gradient_fixtures::rows;
    use tempfile::TempDir;

    fn project_connection(name: &str) -> MGitHostConnection {
        MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            project: Some(project_id()),
            name: name.into(),
            display_name: name.into(),
            git_host_type: GitHostType::Gitea,
            base_url: Some("https://git.example.com".into()),
            receives_events: true,
            reports_status: true,
            created_by: rows::user().id,
            created_at: now(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn declared_connection_and_report_status_replace_an_automatic_link() {
        let db = TestDatabase::create().await;
        let connection = db.connection();
        rows::user()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        rows::project()
            .into_active_model()
            .insert(&connection)
            .await
            .unwrap();
        let declared = project_connection("declared");
        let linked = project_connection("linked");
        for row in [&declared, &linked] {
            row.clone()
                .into_active_model()
                .insert(&connection)
                .await
                .unwrap();
        }
        task::Model {
            git_host_connection: Some(linked.id),
            report_status: true,
            ..rows::task()
        }
        .into_active_model()
        .insert(&connection)
        .await
        .unwrap();

        let dir = TempDir::new().unwrap();
        let crypt_file = dir.path().join("crypt");
        std::fs::write(&crypt_file, "this-is-a-32-byte-crypt-key!!!!").unwrap();
        let transaction = sea_orm::TransactionTrait::begin(&connection).await.unwrap();
        let app = StateApplicator {
            db: &transaction,
            crypt_secret_file: crypt_file.to_str().unwrap(),
            credentials_dir: dir.path(),
            email_enabled: false,
            user_url_destination_allowlist: &Default::default(),
        };
        let state_task: StateTask = serde_json::from_value(serde_json::json!({
            "name": "test-task",
            "project": "test-project",
            "display_name": "Test Task",
            "repository": "https://git.example.com/test/repo",
            "created_by": rows::user().username,
            "git_host_connection": "declared",
            "report_status": false,
        }))
        .unwrap();
        app.apply_tasks(&HashMap::from([(state_task.name.clone(), state_task)]))
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let task = db.rows::<ETask>().await.remove(0);
        assert_eq!(task.git_host_connection, Some(declared.id));
        assert!(!task.report_status);
        assert!(task.managed);
        db.finish().await;
    }
}
