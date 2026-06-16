/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type ActionType = 'send_mail' | 'send_web_request' | 'forge_status_report' | 'open_pr';

export type PrGenerator = 'flake_lock';
export type PrGranularity = 'per_run' | 'per_input';
export type PrVerifyGate = 'none' | 'eval' | 'build';

export type ActionConfig =
  | { type: 'send_mail'; recipients: string[]; subject_template?: string }
  | { type: 'send_web_request'; url: string; token?: string }
  | { type: 'forge_status_report'; integration_id: string }
  | {
      type: 'open_pr';
      integration_id: string;
      generator: PrGenerator;
      granularity: PrGranularity;
      verify_gate: PrVerifyGate;
      branch_pattern: string;
      title_template?: string;
      body_template?: string;
      update_existing: boolean;
    };

export interface Action {
  id: string;
  name: string;
  action_type: ActionType;
  config: ActionConfig;
  events: string[];
  active: boolean;
  last_fired_at: string | null;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface CreateActionRequest {
  name: string;
  config: ActionConfig;
  events?: string[];
  active?: boolean;
}

export interface CreateActionResponse {
  action: Action;
  token?: string;
}

export interface UpdateActionRequest {
  name?: string;
  config?: ActionConfig;
  events?: string[];
  active?: boolean;
}

export interface ActionDelivery {
  id: string;
  event: string;
  success: boolean;
  response_status: number | null;
  error_message: string | null;
  duration_ms: number;
  delivered_at: string;
}

export interface ActionDeliveryDetail extends ActionDelivery {
  request_body: string;
  response_body: string | null;
}

export const ACTION_EVENTS: { group: string; value: string; label: string }[] = [
  { group: 'Evaluation', value: 'evaluation.queued',    label: 'Queued' },
  { group: 'Evaluation', value: 'evaluation.started',   label: 'Started' },
  { group: 'Evaluation', value: 'evaluation.building',  label: 'Building' },
  { group: 'Evaluation', value: 'evaluation.waiting',   label: 'Waiting' },
  { group: 'Evaluation', value: 'evaluation.completed', label: 'Completed' },
  { group: 'Evaluation', value: 'evaluation.failed',    label: 'Failed' },
  { group: 'Evaluation', value: 'evaluation.aborted',   label: 'Aborted' },
  { group: 'Build',      value: 'build.queued',         label: 'Queued' },
  { group: 'Build',      value: 'build.started',        label: 'Started' },
  { group: 'Build',      value: 'build.completed',      label: 'Completed' },
  { group: 'Build',      value: 'build.failed',         label: 'Failed' },
  { group: 'Build',      value: 'build.substituted',    label: 'Substituted' },
];

export const FORGE_STATUS_EVENTS = ['build.started', 'build.completed', 'build.failed'];
