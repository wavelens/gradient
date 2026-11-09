/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { MissingPermissionLoadStateError } from '@gradient/ui/ui';
import type { CachePermission, Permission } from '@core/models';

type Owner = 'project' | 'cache' | 'team';

/// These entries match the `view_mask`, `write_mask`, and cache mask in `backend/gradient-types/src/permissions.rs`.
const minimumBuiltInRoleByPermissionWireName: Record<Permission | CachePermission, 'View' | 'Write' | 'Admin'> = {
  viewProject: 'View',
  manageProjectSettings: 'Admin',
  deleteProject: 'Admin',
  manageMembers: 'Admin',
  manageRoles: 'Admin',
  manageIntegrations: 'Write',
  manageActions: 'Write',
  manageWorkers: 'Write',
  manageSubscriptions: 'Write',
  manageSshKey: 'Write',
  createTask: 'Write',
  editTask: 'Write',
  triggerEvaluation: 'Write',
  manageTriggers: 'Write',
  manageWebhooks: 'Write',
  viewCache: 'View',
  readStore: 'View',
  writeStore: 'Write',
  manageCacheSettings: 'Admin',
  manageCacheKeys: 'Admin',
  manageUpstreamCaches: 'Admin',
  manageCacheMembers: 'Admin',
  manageCacheRoles: 'Admin',
  manageCacheSubscriptions: 'Admin',
  deleteCache: 'Admin',
  manageCacheWebhooks: 'Admin',
};

export function getMembersWithPermissionPhrase(owner: Owner, permission?: Permission | CachePermission): string {
  switch (permission ? minimumBuiltInRoleByPermissionWireName[permission] : 'Admin') {
    case 'View':
      return `members of this ${owner}`;
    case 'Write':
      return `members with Write or Admin on this ${owner}`;
    case 'Admin':
      return `admins of this ${owner}`;
  }
}

export function createMissingPermissionLoadStateError(
  owner: 'project' | 'cache',
  isViewerMemberOfOwner = true,
  permission?: Permission | CachePermission,
): MissingPermissionLoadStateError {
  return createMissingPermissionLoadStateErrorWithViewerText(owner, isViewerMemberOfOwner ? getMembersWithPermissionPhrase(owner, permission) : `members of this ${owner}`);
}

export function createNoAccessLoadStateError(owner: Owner, group: 'members' | 'admins'): MissingPermissionLoadStateError {
  return createMissingPermissionLoadStateErrorWithViewerText(owner, `${group} of this ${owner}`);
}

function createMissingPermissionLoadStateErrorWithViewerText(owner: Owner, holders: string): MissingPermissionLoadStateError {
  return new MissingPermissionLoadStateError(`Only the ${holders} can view this. Ask an admin of this ${owner} for access.`);
}
