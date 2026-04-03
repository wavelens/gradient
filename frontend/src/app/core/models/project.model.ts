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
  created_by?: string;
  created_at?: string;
  managed: boolean;
  can_edit: boolean;
}

export interface EvaluationSummary {
  id: string;
  commit: string;
  status: EvaluationStatus;
  total_builds: number;
  failed_builds: number;
  created_at: string;
  updated_at: string;
}

export interface EntryPointSummary {
  id: string;
  build_id: string;
  derivation_path: string;
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
  last_evaluations: EvaluationSummary[];
  can_edit: boolean;
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
  error?: string;
}

export type EvaluationStatus =
  | 'Queued'
  | 'Evaluating'
  | 'Building'
  | 'Completed'
  | 'Failed'
  | 'Aborted';
