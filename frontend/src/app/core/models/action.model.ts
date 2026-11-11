/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ActionConfig } from './api.generated';

export type ActionType = ActionConfig['type'];

export const ACTION_TYPE_LABEL: Record<ActionType, string> = {
  send_mail: 'Mail',
  send_web_request: 'HTTP request',
  send_matrix_message: 'Matrix',
  send_slack_message: 'Slack',
  open_pr: 'Pull request',
  start_evaluation: 'Start Evaluation',
};
