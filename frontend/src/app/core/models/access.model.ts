/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { CachePermission, Permission } from './api.generated';

export interface AccessState {
  managed: boolean;
  canEdit: boolean;
  canTrigger: boolean;
}

export interface WithPermissions<P extends Permission | CachePermission> {
  managed: boolean;
  permissions: readonly P[];
}

export function accessFromEntity(e: {
  managed: boolean;
  can_edit: boolean;
  can_trigger?: boolean;
}): AccessState {
  return {
    managed: e.managed,
    canEdit: e.can_edit,
    canTrigger: e.can_trigger ?? e.can_edit,
  };
}

/// Grant access to a control only to a caller with every listed permission on the project, task, or cache.
export function accessTo<P extends Permission | CachePermission>(
  holder: WithPermissions<P> | null | undefined,
  ...required: [P, ...P[]]
): AccessState {
  const granted = !!holder && required.every((permission) => holder.permissions.includes(permission));
  return { managed: holder?.managed ?? false, canEdit: granted, canTrigger: granted };
}
