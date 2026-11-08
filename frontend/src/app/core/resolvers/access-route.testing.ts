/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ActivatedRoute, convertToParamMap } from '@angular/router';
import { of } from 'rxjs';
import type { AccessState, CachePermission, CacheResponse, Permission, ProjectResponse, TaskDetailsResponse } from '@core/models';

export const EVERY_PERMISSION: Permission[] = [
  'viewProject',
  'manageProjectSettings',
  'deleteProject',
  'manageMembers',
  'manageRoles',
  'manageIntegrations',
  'manageActions',
  'manageWorkers',
  'manageSubscriptions',
  'manageSshKey',
  'createTask',
  'editTask',
  'triggerEvaluation',
  'manageTriggers',
  'manageWebhooks',
];

export const EVERY_CACHE_PERMISSION: CachePermission[] = [
  'viewCache',
  'readStore',
  'writeStore',
  'manageCacheSettings',
  'manageCacheKeys',
  'manageUpstreamCaches',
  'manageCacheMembers',
  'manageCacheRoles',
  'manageCacheSubscriptions',
  'deleteCache',
  'manageCacheWebhooks',
];

export function projectRouteStub(
  project: string,
  access: AccessState,
  fields: Partial<ProjectResponse> = {},
  query: Record<string, string> = {},
): Partial<ActivatedRoute> {
  const permissions: Permission[] = access.canEdit ? EVERY_PERMISSION : ['viewProject'];
  const role = access.canEdit ? 'Admin' : 'View';
  const resolved = { id: 'project-uuid', name: project, managed: access.managed, permissions, role, ...fields } as ProjectResponse;
  const data = { projectAccess: { project: resolved } };
  return { snapshot: { paramMap: convertToParamMap({ project }), queryParamMap: convertToParamMap(query), data } as never, data: of(data) };
}

export function taskAccessData(access: AccessState, task: Partial<TaskDetailsResponse> = {}) {
  const permissions: Permission[] = [
    ...(access.canEdit ? EVERY_PERMISSION.filter((p) => p !== 'triggerEvaluation') : ['viewProject' as const]),
    ...(access.canTrigger ? ['triggerEvaluation' as const] : []),
  ];
  return { taskAccess: { task: { managed: access.managed, permissions, ...task } as TaskDetailsResponse, access } };
}

export function cacheAccessData(access: AccessState, cache: Partial<CacheResponse> = {}) {
  const permissions: CachePermission[] = access.canEdit ? EVERY_CACHE_PERMISSION : ['viewCache', 'readStore'];
  return { cacheAccess: { cache: { ...cache, managed: access.managed, permissions } as CacheResponse, access } };
}
