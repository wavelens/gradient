/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TaskGitHostChain } from '@core/models';

export const taskRepositoryStatusRepositoryReadableWithoutGitHostConnection: TaskGitHostChain = {
  repository: { state: 'ok', detail: 'acme/app on forgejo.example.com' },
  connection: { state: 'attention', detail: 'No connection to forgejo.example.com' },
  incoming_events: { state: 'off', detail: 'Connection required' },
  commit_status: { state: 'off', detail: 'Connection required' },
};
