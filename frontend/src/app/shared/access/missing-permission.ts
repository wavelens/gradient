/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { CachePermission, Permission } from '@core/models';
import { permissionLabel } from '@shared/text';

export function missingPermissionText(permission: Permission | CachePermission, owner: 'project' | 'cache'): string {
  return `This role doesn't include the ${permissionLabel(permission)} permission for this ${owner}. Ask a ${owner} admin.`;
}
