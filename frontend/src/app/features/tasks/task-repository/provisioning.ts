/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TaskRepositoryResponse } from '@core/models';

export function canInstallWebhookOrDeployKeyForTask(state: TaskRepositoryResponse): boolean {
  const connection = state.git_host_connection;
  return state.can_manage_integrations && !!connection && connection.git_host_type !== 'github' && connection.reports_status;
}
