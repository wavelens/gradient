/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';

export interface DispatchedJobSummary {
  id: string;
  kind: number;
  organization: string;
  worker_id: string;
  score: number;
  dispatched_at: string;
  build_id: string | null;
  evaluation_id: string;
}

export interface DispatchedJobsResponse {
  jobs: DispatchedJobSummary[];
  other_running: number;
}

export interface DispatchedJobDetail extends DispatchedJobSummary {
  queued_at: string;
  finished_at: string | null;
  score_breakdown: { rules: Record<string, number>; total: number };
  worker_context: Record<string, unknown>;
  job_context: Record<string, unknown>;
  candidates: unknown | null;
}

export interface BoardWorker {
  id: string | null;
  organization: string | null;
  draining: boolean;
  assigned_jobs: number;
  max_concurrent_builds: number;
  eval: boolean;
  fetch: boolean;
  build: boolean;
  architectures: string[];
  cpu_usage_pct: number | null;
  ram_free_mb: number | null;
  ram_total_mb: number | null;
}

export interface ExpensiveBuild {
  build_id: string;
  organization: string;
  name: string;
  build_time_ms: number;
  worker: string | null;
}

export interface MetricPoint {
  bucket_start: string;
  count: number;
  sum: number;
  min: number;
  max: number;
  avg: number;
}

export interface MetricMeta {
  key: string;
  kind: string;
  unit: string;
  dimensions: string[];
}

export interface ScoreBucket {
  lo: number;
  hi: number;
  count: number;
}

export interface RuleContribution {
  rule: string;
  avg: number;
  min: number;
  max: number;
}

export interface ScoringSummary {
  sample_size: number;
  score_min: number;
  score_max: number;
  score_avg: number;
  histogram: ScoreBucket[];
  rules: RuleContribution[];
}

@Injectable({ providedIn: 'root' })
export class BoardService {
  private api = inject(ApiService);

  getDispatchedJobs(): Observable<DispatchedJobsResponse> {
    return this.api.get<DispatchedJobsResponse>('board/jobs/dispatched');
  }

  getJob(id: string): Observable<DispatchedJobDetail> {
    return this.api.get<DispatchedJobDetail>(`board/jobs/${id}`);
  }

  getWorkers(): Observable<BoardWorker[]> {
    return this.api.get<BoardWorker[]>('board/workers');
  }

  getExpensive(windowDays = 30, excludeAcknowledged = true): Observable<ExpensiveBuild[]> {
    return this.api.get<ExpensiveBuild[]>(
      `board/jobs/expensive?window_days=${windowDays}&exclude_acknowledged=${excludeAcknowledged}`
    );
  }

  getCatalog(): Observable<MetricMeta[]> {
    return this.api.get<MetricMeta[]>('metrics/catalog');
  }

  getScoringSummary(windowHours = 24): Observable<ScoringSummary> {
    return this.api.get<ScoringSummary>(`board/scoring/summary?window_hours=${windowHours}`);
  }

  query(metric: string, granularity = 'hour', org?: string): Observable<MetricPoint[]> {
    let url = `metrics/query?metric=${encodeURIComponent(metric)}&granularity=${granularity}`;
    if (org) {
      url += `&org=${org}`;
    }
    return this.api.get<MetricPoint[]>(url);
  }
}
