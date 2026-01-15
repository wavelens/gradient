/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
  last_check_at?: string;
  force_evaluation: boolean;
  created_by?: string;
  created_at?: string;
}

export interface EvaluationSummary {
  id: string;
  status: EvaluationStatus;
  total_builds: number;
  failed_builds: number;
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
}

export interface Evaluation {
  id: string;
  project?: string;
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
