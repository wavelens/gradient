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

export interface SeriesPoint {
  bucket_start: string;
  count: number;
  sum: number;
}

export interface BoardCacheStats {
  totals: {
    bytes: number;
    nar_bytes: number;
    packages: number;
    bytes_sent_total: number;
    requests_total: number;
  };
  traffic: SeriesPoint[];
  storage: SeriesPoint[];
}

export interface HttpRouteStat {
  method: string;
  route: string;
  count: number;
  avg_ms: number;
  errors: number;
}

export interface WorkerNet {
  worker_id: string | null;
  network_speed_mbps: number | null;
  disk_speed_mbps: number | null;
}

export interface BoardNetworkStats {
  nar_egress: SeriesPoint[];
  workers: WorkerNet[];
  http: HttpRouteStat[];
}

export interface BoardFleetPoint {
  bucket_start: string;
  connected: number;
  draining: number;
  eval: number;
  fetch: number;
  build: number;
}

export interface ProcessStat {
  resident_memory_bytes: number;
  virtual_memory_bytes: number;
  open_fds: number;
  max_fds: number;
  cpu_seconds_total: number;
  threads: number;
}

export interface BoardHealth {
  version: string;
  uptime_seconds: number;
  workers_connected: number;
  jobs_pending: number;
  jobs_active: number;
  cache_bytes: number;
  cache_packages: number;
  process: ProcessStat;
  http: HttpRouteStat[];
  rollup_lag_seconds: number | null;
  latest_rollup_bucket: string | null;
}

export interface DurationsHeatmap {
  times: string[];
  bands: { band: string; counts: number[] }[];
}

export interface TopOrgBuildTime {
  organization: string;
  total_build_ms: number;
  build_count: number;
}

export interface ExpensiveResource {
  derivation: string;
  organization: string;
  name: string;
  value: number;
  unit: string;
  worker: string;
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

  getCache(windowHours = 24): Observable<BoardCacheStats> {
    return this.api.get<BoardCacheStats>(`board/cache?window_hours=${windowHours}`);
  }

  getNetwork(windowHours = 24): Observable<BoardNetworkStats> {
    return this.api.get<BoardNetworkStats>(`board/network?window_hours=${windowHours}`);
  }

  getFleet(windowHours = 24): Observable<BoardFleetPoint[]> {
    return this.api.get<BoardFleetPoint[]>(`board/fleet?window_hours=${windowHours}`);
  }

  getHealth(): Observable<BoardHealth> {
    return this.api.get<BoardHealth>('board/health');
  }

  getDurationsHeatmap(windowHours = 24): Observable<DurationsHeatmap> {
    return this.api.get<DurationsHeatmap>(`board/durations/heatmap?window_hours=${windowHours}`);
  }

  getTopOrgs(windowDays = 30): Observable<TopOrgBuildTime[]> {
    return this.api.get<TopOrgBuildTime[]>(`board/expensive/top-orgs?window_days=${windowDays}`);
  }

  getExpensiveByResource(
    metric: 'ram' | 'cpu' | 'disk' | 'network',
    windowDays = 30,
    excludeAcknowledged = true
  ): Observable<ExpensiveResource[]> {
    return this.api.get<ExpensiveResource[]>(
      `board/jobs/expensive-by-resource?metric=${metric}&window_days=${windowDays}&exclude_acknowledged=${excludeAcknowledged}`
    );
  }

  query(metric: string, granularity = 'hour', org?: string): Observable<MetricPoint[]> {
    let url = `metrics/query?metric=${encodeURIComponent(metric)}&granularity=${granularity}`;
    if (org) {
      url += `&org=${org}`;
    }
    return this.api.get<MetricPoint[]>(url);
  }
}
