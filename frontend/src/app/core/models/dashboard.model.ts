/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EvaluationStatus } from './task.model';

export type Tier = 'starred_active' | 'active' | 'starred' | 'member';
export type DashboardFilter = 'all' | 'failing' | 'worse' | 'starred';

export interface HistoryBar {
  id: string;
  status: EvaluationStatus;
  duration_ms: number | null;
  created_at: string;
}

export interface TaskRow {
  project: string;
  task: string;
  starred: boolean;
  tier: Tier;
  latest: { id: string; status: EvaluationStatus; commit: string; created_at: string } | null;
  entry_points: { ok: number; failing: number; total: number } | null;
  delta: number | null;
  speed_ms: number | null;
  reliability: number | null;
  evaluations_per_week: number;
  history: HistoryBar[];
}

export interface TasksPage {
  counts: Record<DashboardFilter, number>;
  total: number;
  tasks: TaskRow[];
}

export interface DashboardStats {
  cpu_time_ms: number;
  cpu_time_ms_7d: number;
  builds_completed: number;
  cache_size_bytes: number;
  workers: { online: number; busy_pct: number };
  queue_wait_p50_ms: number;
}

export interface ActivityDay {
  date: string;
  evaluations: number;
  failed: number;
}

export interface RailProject {
  name: string;
  display_name: string;
  starred: boolean;
  tier: Tier;
  status: EvaluationStatus | null;
  task_count: number;
  tasks?: { name: string; status: EvaluationStatus | null }[];
}

export interface RailCache {
  name: string;
  display_name: string;
  starred: boolean;
  nar_count: number;
}

export interface Rail {
  projects: RailProject[];
  caches: RailCache[];
  operations: boolean;
}

export interface SearchHit {
  kind: 'project' | 'task' | 'cache' | 'nar' | 'commit';
  label: string;
  sublabel: string;
  route: string;
  starred: boolean;
}

export type StarTarget =
  | { kind: 'project'; project: string }
  | { kind: 'task'; project: string; task: string }
  | { kind: 'cache'; cache: string };
