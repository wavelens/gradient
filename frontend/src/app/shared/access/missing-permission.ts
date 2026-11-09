/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { MissingPermissionLoadStateError } from '@gradient/ui/ui';
import type { CachePermission, Permission } from '@core/models';
import { permissionLabel } from '@shared/text';

export function createMissingPermissionLoadStateError(
  permission: Permission | CachePermission,
  owner: 'project' | 'cache',
  isViewerMemberOfOwner = true,
): MissingPermissionLoadStateError {
  if (!isViewerMemberOfOwner) return createNoAccessLoadStateError(owner, 'members');
  const label = permissionLabel(permission);
  return new MissingPermissionLoadStateError(`The ${label} permission for this ${owner} is necessary. Contact the ${owner} administrator.`);
}

export function createNoAccessLoadStateError(owner: 'project' | 'cache' | 'team', group: 'members' | 'admins'): MissingPermissionLoadStateError {
  return new MissingPermissionLoadStateError(`Only the ${group} of this ${owner} can view this. Ask an admin of this ${owner} for access.`);
}
