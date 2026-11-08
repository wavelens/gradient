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
): MissingPermissionLoadStateError {
  return new MissingPermissionLoadStateError(
    `This role doesn't include the ${permissionLabel(permission)} permission for this ${owner}. Ask a ${owner} admin.`,
  );
}

export function createNoAccessLoadStateError(owner: 'project' | 'team', group: 'members' | 'admins'): MissingPermissionLoadStateError {
  return new MissingPermissionLoadStateError(`Only the ${group} of this ${owner} can view this. Ask an admin of this ${owner} for access.`);
}
