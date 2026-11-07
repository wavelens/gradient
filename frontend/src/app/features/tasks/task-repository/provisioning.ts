/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { RepositoryIntegration, TaskRepositoryResponse } from '@core/models';

export function provisioningIntegration(state: TaskRepositoryResponse): RepositoryIntegration | null {
  if (!state.can_manage_integrations) return null;
  const preferred = state.report_status?.integration_id;
  const candidates = state.integrations.filter(
    (i) => i.kind === 'outbound' && i.serves_repository && i.has_access_token && i.git_host_type !== 'github'
  );
  return candidates.find((i) => i.id === preferred) ?? candidates[0] ?? null;
}
