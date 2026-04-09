/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BuildStatus, Architecture } from './build.model';

export interface Project {
  id: string;
  organization: string;
  name: string;
  active: boolean;
  display_name: string;
  description: string;
  repository: string;
  evaluation_wildcard: string;
  last_evaluation?: string;
  last_evaluation_status?: EvaluationStatus;
  last_check_at?: string;
  force_evaluation: boolean;
  keep_evaluations: number;
  created_by?: string;
  created_at?: string;
  managed: boolean;
  can_edit: boolean;
  ci_reporter_type?: string | null;
  ci_reporter_url?: string | null;
}

export interface EvaluationSummary {
  id: string;
  commit: string;
  status: EvaluationStatus;
  total_builds: number;
  failed_builds: number;
  completed_entry_points: number;
  failed_entry_points: number;
  entry_point_diff: number | null;
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
  evaluation_id: string;
  evaluation_status: EvaluationStatus;
  created_at: string;
}

export interface ProjectDetail {
  id: string;
  name: string;
  display_name: string;
  description: string;
  repository: string;
  evaluation_wildcard: string;
  active: boolean;
  created_at: string;
  keep_evaluations: number;
  last_evaluations: EvaluationSummary[];
  can_edit: boolean;
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
  repository: string;
  commit: string;
  wildcard: string;
  status: EvaluationStatus;
  previous?: string;
  next?: string;
  created_at: string;
  error_count: number;
  warning_count: number;
}

export type EvaluationStatus =
  | 'Queued'
  | 'EvaluatingFlake'
  | 'EvaluatingDerivation'
  | 'Building'
  | 'Waiting'
  | 'Completed'
  | 'Failed'
  | 'Aborted';
