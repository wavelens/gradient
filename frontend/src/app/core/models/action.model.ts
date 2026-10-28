/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ActionConfig } from './api.generated';

export type ActionType = ActionConfig['type'];

export const ACTION_TYPE_LABEL: Record<ActionType, string> = {
  send_mail: 'Mail',
  send_web_request: 'Webhook',
  send_matrix_message: 'Matrix',
  send_slack_message: 'Slack',
  git_host_status_report: 'Git Status',
  open_pr: 'PR',
  start_evaluation: 'Start Evaluation',
};

export const GIT_HOST_STATUS_EVENTS = [
  'build.started',
  'build.completed',
  'build.failed',
  'exec_job.started',
  'exec_job.completed',
  'exec_job.failed',
  'exec_job.aborted',
];
