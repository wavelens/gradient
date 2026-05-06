/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type TriggerType = 'polling' | 'reporter_push' | 'reporter_pull_request' | 'time';
export type ConcurrencyPolicy = 'hard_abort' | 'soft_abort' | 'allow' | 'skip';

export interface PollingTriggerConfig {
  interval_secs: number;
}

export interface ReporterPushTriggerConfig {
  integration_id: string;
  branches?: string[];
  tags?: string[];
  releases_only?: boolean;
}

export interface ReporterPullRequestTriggerConfig {
  integration_id: string;
  branches?: string[];
  actions?: string[];
}

export interface TimeTriggerConfig {
  cron: string;
}

export type TriggerConfig =
  | PollingTriggerConfig
  | ReporterPushTriggerConfig
  | ReporterPullRequestTriggerConfig
  | TimeTriggerConfig;

export interface ProjectTrigger {
  id: string;
  project: string;
  type: TriggerType;
  concurrency: ConcurrencyPolicy;
  config: TriggerConfig;
  active: boolean;
  last_fired_at: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateTriggerBody {
  config: TriggerConfig;
  concurrency: ConcurrencyPolicy;
  active?: boolean;
}

export interface UpdateTriggerBody {
  config?: TriggerConfig;
  concurrency?: ConcurrencyPolicy;
  active?: boolean;
}
