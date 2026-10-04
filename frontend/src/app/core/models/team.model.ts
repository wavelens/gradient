/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ConnectionFailure } from './worker.model';
import { EvaluationStatus } from './task.model';

export type TeamRole = 'admin' | 'member';

export interface TeamSummary {
  name: string;
  display_name: string;
  role: TeamRole;
}

export interface Team {
  id: string;
  name: string;
  display_name: string;
  managed: boolean;
  role: TeamRole | null;
  oidc_group: string | null;
  scim_group: string | null;
  new_project_users: boolean;
  new_project_workers: boolean;
  new_project_role: string | null;
}

export interface PatchTeam {
  display_name?: string;
  new_project_users?: boolean;
  new_project_workers?: boolean;
  new_project_role?: string;
  oidc_group?: string;
  scim_group?: string;
}

export interface TeamMember {
  user: string;
  name: string;
  role: TeamRole;
  via_group: boolean;
}

export interface TeamWorker {
  worker_id: string;
  display_name: string;
  registered_at: string;
  active: boolean;
  managed: boolean;
  url?: string;
  gradient_ci: boolean;
  enable_fetch: boolean;
  enable_eval: boolean;
  enable_build: boolean;
  connected: boolean;
  last_error?: ConnectionFailure;
}

export interface RegisterTeamWorker {
  worker_id: string;
  display_name: string;
  url?: string;
}

export interface TeamGrant {
  team: string;
  display_name: string;
  role: string | null;
  users: boolean;
  workers: boolean;
  pending: boolean;
}

export interface GrantRequest {
  team: string;
  role?: string;
  users: boolean;
  workers: boolean;
}

export interface TeamGrants {
  projects: { project: string; display_name: string; role: string | null; users: boolean; workers: boolean }[];
  caches: { cache: string; display_name: string; role: string }[];
}

export interface TeamEvaluation {
  id: string;
  project: string;
  task: string;
  status: EvaluationStatus;
  created_at: string;
}

export interface TeamRequest {
  id: string;
  kind: 'project' | 'cache';
  target: string;
  display_name: string;
  role: string | null;
  users: boolean;
  workers: boolean;
  requested_by: string | null;
  created_at: string;
}
