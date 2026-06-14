/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BuildStatus, Architecture } from './build.model';
import { ConcurrencyPolicy, TriggerType } from './trigger.model';

export interface Project {
  id: string;
  organization: string;
  name: string;
  active: boolean;
  display_name: string;
  description: string;
  repository: string;
  wildcard: string;
  last_evaluation?: string;
  last_evaluation_status?: EvaluationStatus;
  last_check_at?: string;
  force_evaluation: boolean;
  keep_evaluations: number;
  concurrency: ConcurrencyPolicy;
  sign_cache: boolean;
  created_by?: string;
  created_at?: string;
  managed: boolean;
  can_edit: boolean;
  can_trigger: boolean;
}

export interface BuildStatusCounts {
  completed: number;
  failed: number;
  building: number;
  queued: number;
  substituted: number;
  aborted: number;
}

export interface QueueSummary {
  building: number;
  queued: number;
}

export interface EvaluationSummary {
  id: string;
  commit: string;
  commit_message: string | null;
  status: EvaluationStatus;
  trigger: { id: string; type: TriggerType } | null;
  triggered_by: string | null;
  total_builds: number;
  builds: BuildStatusCounts;
  errors: number;
  warnings: number;
  created_at: string;
  updated_at: string;
}

export interface EntryPointSummary {
  id: string;
  build_id: string;
  derivation_path: string;
  eval: string;
  build_status: BuildStatus;
  has_artefacts: boolean;
  architecture: Architecture;
  build_time_ms: number | null;
  deps: BuildStatusCounts;
  deps_total: number | null;
  created_at: string;
}

export interface ProjectDetail {
  id: string;
  name: string;
  display_name: string;
  description: string;
  repository: string;
  wildcard: string;
  active: boolean;
  created_at: string;
  keep_evaluations: number;
  last_evaluations: EvaluationSummary[];
  last_check_at: string | null;
  queue: QueueSummary;
  can_edit: boolean;
  can_trigger: boolean;
  managed: boolean;
}

export interface EvaluationMessage {
  id: string;
  level: 'Error' | 'Warning' | 'Notice';
  message: string;
  source?: string;
  created_at: string;
  entry_points: string[];
}

export interface Evaluation {
  id: string;
  project?: string;
  project_name?: string;
  project_display_name?: string;
  repository: string;
  commit: string;
  wildcard: string;
  status: EvaluationStatus;
  previous?: string;
  next?: string;
  created_at: string;
  updated_at: string;
  error_count: number;
  warning_count: number;
  waiting_reason?: WaitingReason;
  trigger: { id: string; type: TriggerType } | null;
}

export type WaitingReason =
  | WorkersWaitingReason
  | EvalWorkersWaitingReason
  | ApprovalWaitingReason
  | NoCacheWaitingReason
  | CacheStorageFullWaitingReason;

export interface WorkersWaitingReason {
  kind: 'workers';
  unmet: UnmetRequirement[];
  connected_workers: number;
  available_architectures: string[];
}

export type EvalCapability = 'fetch' | 'eval';

export interface EvalWorkersWaitingReason {
  kind: 'eval_workers';
  capability: EvalCapability;
  connected_workers: number;
}

export interface ApprovalWaitingReason {
  kind: 'approval';
  pr_number: number;
  pr_author: string;
}

export interface NoCacheWaitingReason {
  kind: 'no_cache';
}

export interface CacheStorageFullWaitingReason {
  kind: 'cache_storage_full';
}

export interface UnmetRequirement {
  architecture: string;
  required_features: string[];
  build_count: number;
}

export type EvaluationStatus =
  | 'Queued'
  | 'Fetching'
  | 'EvaluatingFlake'
  | 'EvaluatingDerivation'
  | 'Building'
  | 'Waiting'
  | 'Completed'
  | 'Failed'
  | 'Aborted';
