/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod integrations;
pub mod management;
pub mod members;
pub mod roles;
pub mod settings;
pub mod ssh;
pub mod workers;

pub use self::integrations::{
    CreateIntegrationRequest, IntegrationResponse, IntegrationSummaryResponse,
    PatchIntegrationRequest, delete_integration, get_integration, get_integration_summaries,
    get_integrations, patch_integration, put_integration,
};
pub use self::management::{
    MakeProjectRequest, PatchProjectRequest, ProjectResponse, ProjectSummary, delete_project, get,
    get_project, get_project_name_available, get_public_projects, patch_project, put,
};
pub use self::members::{
    AddUserRequest, RemoveUserRequest, StringListItem, delete_project_users, get_project_users,
    patch_project_users, post_project_users,
};
pub use self::roles::{
    CreateRoleRequest, PatchRoleRequest, RoleListResponse, RoleResponse, delete_project_role,
    get_project_role, get_project_roles, patch_project_role, post_project_role,
};
pub use self::settings::{
    CacheSubscriptionItem, SubscribeCacheRequest, delete_project_public,
    delete_project_subscribe_cache, get_project_subscribe, post_project_public,
    post_project_subscribe_cache,
};
pub use self::ssh::{get_project_ssh, post_project_ssh};
pub use self::workers::{
    PatchWorkerRequest, ProjectWorkerEntry, RegisterWorkerRequest, RegisterWorkerResponse,
    WorkerLiveInfo, WorkerTestResponse, delete_project_worker, get_project_worker_metrics,
    get_project_workers, patch_project_worker, post_project_worker, post_project_worker_test,
};
