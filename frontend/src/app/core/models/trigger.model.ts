/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ForgeType } from './integration.model';

export type TriggerType = 'polling' | 'reporter_push' | 'reporter_pull_request' | 'time';
export type ConcurrencyPolicy = 'hard_abort' | 'soft_abort' | 'all' | 'skip';

/** Inlined integration handle on reporter trigger responses. Mirrors the
 *  backend `TriggerIntegrationSummary` — `null` for polling/time triggers
 *  and for orphaned references (integration row deleted). */
export interface TriggerIntegrationRef {
  id: string;
  name: string;
  display_name: string;
  forge_type: ForgeType;
}

export interface PollingTriggerConfig {
  type: 'polling';
  interval_secs: number;
  /** Branch to poll (e.g. "main"). Leave undefined/null to poll the remote HEAD. */
  branch?: string | null;
}

export interface ReporterPushTriggerConfig {
  type: 'reporter_push';
  integration_id: string;
  branches?: string[];
  tags?: string[];
  releases_only?: boolean;
}

export interface ReporterPullRequestTriggerConfig {
  type: 'reporter_pull_request';
  integration_id: string;
  branches?: string[];
  actions?: string[];
  /** When true (default), PRs from non-writer contributors are parked until
   *  a maintainer approves them via the forge's check-run action (GitHub)
   *  or a `/ci run` comment (Gitea/Forgejo/GitLab). */
  require_approval?: boolean;
}

export interface TimeTriggerConfig {
  type: 'time';
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
  config: TriggerConfig;
  active: boolean;
  last_fired_at: string | null;
  created_at: string;
  updated_at: string;
  /** Populated by the backend for `reporter_push` / `reporter_pull_request`
   *  triggers when the referenced integration still exists. */
  integration: TriggerIntegrationRef | null;
}

export interface CreateTriggerBody {
  config: TriggerConfig;
  active?: boolean;
}

export interface UpdateTriggerBody {
  config?: TriggerConfig;
  active?: boolean;
}
