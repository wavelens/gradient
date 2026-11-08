/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { Permission, WithPermissions } from '@core/models';

const projectSettingsChangePermissions: readonly Permission[] = [
  'manageProjectSettings',
  'deleteProject',
  'manageMembers',
  'manageRoles',
  'manageIntegrations',
  'manageWorkers',
  'manageSubscriptions',
  'manageSshKey',
  'manageWebhooks',
];

const taskSettingsChangePermissions: readonly Permission[] = ['editTask', 'manageTriggers', 'manageActions'];

export function canOpenProjectSettings(projectOrTask: Pick<WithPermissions<Permission>, 'permissions'> | null | undefined): boolean {
  return projectSettingsChangePermissions.some((permission) => projectOrTask?.permissions.includes(permission));
}

export function canOpenTaskSettings(task: Pick<WithPermissions<Permission>, 'permissions'> | null | undefined): boolean {
  return taskSettingsChangePermissions.some((permission) => task?.permissions.includes(permission));
}
