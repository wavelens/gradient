/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::webhook::WebhookScope;
use gradient_types::events::{EventFilter, EventOwner};
use gradient_types::*;
use sea_orm::{ColumnTrait, Condition, ConnectionTrait, DbErr, EntityTrait, QueryFilter};

/// Personal events such as logins, keys and sessions are reaching instance webhooks only.
pub fn routes_to(hook: &MWebhook, owner: &EventOwner, name: &str, personal: bool) -> bool {
    let patterns = serde_json::from_value::<Vec<String>>(hook.events.clone()).unwrap_or_default();
    if !hook.active || !EventFilter::from_patterns(patterns).matches(name) {
        return false;
    }
    match hook.scope {
        WebhookScope::Instance => true,
        _ if personal => false,
        WebhookScope::Project => hook.project.is_some() && hook.project == owner.project,
        WebhookScope::Cache => hook.cache.is_some() && hook.cache == owner.cache,
        WebhookScope::Team => hook.team.is_some() && hook.team == owner.team,
    }
}

pub async fn candidates<C: ConnectionTrait>(
    db: &C,
    owner: &EventOwner,
) -> Result<Vec<MWebhook>, DbErr> {
    let mut scope = Condition::any().add(CWebhook::Scope.eq(WebhookScope::Instance));
    if let Some(project) = owner.project {
        scope = scope.add(CWebhook::Project.eq(project));
    }
    if let Some(cache) = owner.cache {
        scope = scope.add(CWebhook::Cache.eq(cache));
    }
    if let Some(team) = owner.team {
        scope = scope.add(CWebhook::Team.eq(team));
    }
    EWebhook::find()
        .filter(CWebhook::Active.eq(true))
        .filter(scope)
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn hook(
        scope: WebhookScope,
        project: Option<ProjectId>,
        cache: Option<CacheId>,
        events: &[&str],
    ) -> MWebhook {
        MWebhook {
            scope,
            project,
            cache,
            events: serde_json::json!(events),
            active: true,
            ..Default::default()
        }
    }

    #[test]
    fn project_webhooks_get_their_project_only() {
        let p = ProjectId::new(Uuid::from_u128(1));
        let h = hook(WebhookScope::Project, Some(p), None, &["build.*"]);
        let mine = EventOwner {
            project: Some(p),
            ..Default::default()
        };
        let other = EventOwner {
            project: Some(ProjectId::new(Uuid::from_u128(2))),
            ..Default::default()
        };
        assert!(routes_to(&h, &mine, "build.completed", false));
        assert!(!routes_to(&h, &other, "build.completed", false));
        assert!(!routes_to(&h, &mine, "evaluation.completed", false));
    }

    #[test]
    fn personal_events_reach_only_instance_webhooks() {
        let p = ProjectId::new(Uuid::from_u128(1));
        let owner = EventOwner {
            project: Some(p),
            ..Default::default()
        };
        let project_hook = hook(WebhookScope::Project, Some(p), None, &["*"]);
        let instance_hook = hook(WebhookScope::Instance, None, None, &["*"]);
        assert!(!routes_to(&project_hook, &owner, "api_key.create", true));
        assert!(routes_to(&instance_hook, &owner, "api_key.create", true));
    }

    #[test]
    fn an_inactive_webhook_gets_nothing() {
        let mut h = hook(WebhookScope::Instance, None, None, &[]);
        h.active = false;
        assert!(!routes_to(&h, &EventOwner::default(), "gc.swept", false));
    }

    #[test]
    fn cache_webhooks_get_their_cache_only() {
        let c = CacheId::new(Uuid::from_u128(3));
        let h = hook(WebhookScope::Cache, None, Some(c), &["cache.*"]);
        let owner = EventOwner {
            cache: Some(c),
            ..Default::default()
        };
        assert!(routes_to(&h, &owner, "cache.nar.upload", false));
        assert!(!routes_to(
            &h,
            &EventOwner::default(),
            "cache.nar.upload",
            false
        ));
    }

    fn team_hook(team: TeamId) -> MWebhook {
        MWebhook {
            team: Some(team),
            ..hook(WebhookScope::Team, None, None, &[])
        }
    }

    #[test]
    fn team_hook_receives_its_team_event() {
        let a = TeamId::new(Uuid::from_u128(4));
        assert!(routes_to(
            &team_hook(a),
            &EventOwner::team(a),
            "worker.demand",
            false
        ));
    }

    #[test]
    fn team_hook_ignores_other_team_and_project_events() {
        let h = team_hook(TeamId::new(Uuid::from_u128(4)));
        let other_team = EventOwner::team(TeamId::new(Uuid::from_u128(5)));
        let project = EventOwner::project(ProjectId::new(Uuid::from_u128(1)));
        assert!(!routes_to(&h, &other_team, "worker.demand", false));
        assert!(!routes_to(&h, &project, "build.failed", false));
    }

    #[test]
    fn an_empty_event_list_subscribes_to_everything() {
        let h = hook(WebhookScope::Instance, None, None, &[]);
        assert!(routes_to(&h, &EventOwner::default(), "gc.swept", false));
    }
}
