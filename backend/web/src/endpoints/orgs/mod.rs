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
    MakeOrganizationRequest, OrgResponse, OrganizationSummary, PatchOrganizationRequest,
    delete_organization, get, get_org_name_available, get_organization, get_public_organizations,
    patch_organization, put,
};
pub use self::members::{
    AddUserRequest, RemoveUserRequest, StringListItem, delete_organization_users,
    get_organization_users, patch_organization_users, post_organization_users,
};
pub use self::roles::{
    CreateRoleRequest, PatchRoleRequest, RoleListResponse, RoleResponse, delete_organization_role,
    get_organization_role, get_organization_roles, patch_organization_role, post_organization_role,
};
pub use self::settings::{
    CacheSubscriptionItem, SubscribeCacheRequest, delete_organization_public,
    delete_organization_subscribe_cache, get_organization_subscribe, post_organization_public,
    post_organization_subscribe_cache,
};
pub use self::ssh::{get_organization_ssh, post_organization_ssh};
pub use self::workers::{
    OrgWorkerEntry, PatchWorkerRequest, RegisterWorkerRequest, RegisterWorkerResponse,
    WorkerLiveInfo, delete_org_worker, get_org_worker_stats, get_org_workers, patch_org_worker,
    post_org_worker,
};
