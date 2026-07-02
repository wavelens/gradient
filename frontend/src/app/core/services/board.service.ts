/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable, shareReplay } from 'rxjs';
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
  pname: string | null;
}

export interface DispatchedJobsResponse {
  jobs: DispatchedJobSummary[];
  other_running: number;
}

export interface PendingJobSummary {
  kind: number;
  organization: string;
  evaluation_id: string;
  build_id: string | null;
  queued_at: string;
  dependency_count: number;
  pname: string | null;
}

export interface PendingJobsResponse {
  jobs: PendingJobSummary[];
  other_pending: number;
}

export interface DecisionCandidateView {
  id: string;
  job_id: string;
  kind: number;
  organization: string;
  build_id: string | null;
  evaluation_id: string;
  pname: string | null;
  score: number;
  won: boolean;
}

export interface DispatchDecisionView {
  at: string;
  worker_id: string;
  kind: number;
  winner: string | null;
  candidates: DecisionCandidateView[];
}

export interface GradientCapabilities {
  core: boolean; federate: boolean; fetch: boolean; eval: boolean; build: boolean; cache: boolean;
}

export interface Windowed { w5m: number | null; w1h: number | null; w24h: number | null; }

export interface WorkerContextView {
  architectures: string[];
  system_features: string[];
  capabilities: GradientCapabilities;
  cpu_count: number;
  cpu_core_score: number;
  ram_total_mb: number;
  ram_free_mb: number | null;
  cpu_usage_pct: number | null;
  disk_speed_mbps: number | null;
  network_speed_mbps: number | null;
}

export interface DerivationRef { build_id: string; drv_path: string; pname: string | null; }

export interface JobHistoryView {
  peak_ram_mb: number; avg_cpu_time_ms: number; build_time_ms: number;
  avg_disk_bytes: number; oom_rate: number; samples: number;
}

export interface JobContextView {
  kind: 'Build' | 'Eval';
  architecture: string;
  missing_count: number | null;
  missing_nar_size: number | null;
  org_work_share: number | null;
  rescore_count: number;
  queued_at: string;
  ready_at: string;
  dependency_count?: number;
  pname?: string | null;
  closure_size?: number | null;
  prefer_local_build?: boolean;
  is_fixed_output?: boolean;
  history?: JobHistoryView;
  derivations?: DerivationRef[];
  fetch_flake?: boolean;
}

export interface InstanceContextView {
  wait_secs: Windowed;
  build_time_ms: Windowed;
  peak_ram_mb: Windowed;
  cpu_time_ms: Windowed;
  avg_cpu_pct: Windowed;
  disk_bytes: Windowed;
  network_mbps: Windowed;
  oom_rate: Windowed;
  closure_size: Windowed;
  nar_size_mb: Windowed;
  missing_paths: Windowed;
  dependency_cnt: Windowed;
  completed: Windowed;
  active_builds: number;
  pending_builds: number;
  total_workers: number;
  idle_workers: number;
}

export interface DispatchedJobDetail extends DispatchedJobSummary {
  organization_name: string;
  queued_at: string;
  finished_at: string | null;
  score_breakdown: { rules: Record<string, number>; total: number };
  worker_context: WorkerContextView;
  job_context: JobContextView;
  instance_context: InstanceContextView | null;
  candidates: unknown | null;
  previous_attempts: {
    dispatched_job_id: string;
    substitute: boolean;
    outcome: number;
    reason: number | null;
    failure_message: string | null;
    created_at: string;
  }[];
  passed_over: boolean;
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

export interface RuleDescription {
  rule: string;
  description: string;
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

export interface BoardUpstream {
  upstream_id: string;
  display_name: string;
  url: string;
  avg_latency_ms: number | null;
  hit_rate: number | null;
  requests_total: number;
  latency: SeriesPoint[];
  hit_rate_series: SeriesPoint[];
}

export interface BoardUpstreamStats {
  upstreams: BoardUpstream[];
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
  draining: boolean;
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

export interface ExpensiveEval {
  evaluation: string;
  organization: string;
  name: string;
  value: number;
  unit: string;
  worker: string;
}

export interface FlakeGraphNode {
  path: string;
  parent: string | null;
  name: string;
  kind: string;
  is_derivation: boolean;
  drv_path: string | null;
}

@Injectable({ providedIn: 'root' })
export class BoardService {
  private api = inject(ApiService);
  private scoringRules$?: Observable<RuleDescription[]>;

  getDispatchedJobs(): Observable<DispatchedJobsResponse> {
    return this.api.get<DispatchedJobsResponse>('board/jobs/dispatched');
  }

  getPendingJobs(): Observable<PendingJobsResponse> {
    return this.api.get<PendingJobsResponse>('board/jobs/pending');
  }

  /// Recent dispatch decisions with every candidate's score, including the
  /// rejected/negative ones the dispatcher passed over (superuser-only).
  getDispatchDecisions(): Observable<DispatchDecisionView[]> {
    return this.api.get<DispatchDecisionView[]>('board/jobs/decisions');
  }

  getJob(id: string): Observable<DispatchedJobDetail> {
    return this.api.get<DispatchedJobDetail>(`board/jobs/${id}`);
  }

  getWorkers(): Observable<BoardWorker[]> {
    return this.api.get<BoardWorker[]>('board/workers');
  }

  getExpensive(windowDays = 30): Observable<ExpensiveBuild[]> {
    return this.api.get<ExpensiveBuild[]>(`board/jobs/expensive?window_days=${windowDays}`);
  }

  getCatalog(): Observable<MetricMeta[]> {
    return this.api.get<MetricMeta[]>('metrics/catalog');
  }

  getScoringSummary(windowHours = 24): Observable<ScoringSummary> {
    return this.api.get<ScoringSummary>(`board/scoring/summary?window_hours=${windowHours}`);
  }

  getScoringRules(): Observable<RuleDescription[]> {
    this.scoringRules$ ??= this.api
      .get<RuleDescription[]>('board/scoring/rules')
      .pipe(shareReplay({ bufferSize: 1, refCount: false }));

    return this.scoringRules$;
  }

  getCache(windowHours = 24): Observable<BoardCacheStats> {
    return this.api.get<BoardCacheStats>(`board/cache?window_hours=${windowHours}`);
  }

  getUpstreams(windowHours = 24): Observable<BoardUpstreamStats> {
    return this.api.get<BoardUpstreamStats>(`board/cache/upstreams?window_hours=${windowHours}`);
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
    windowDays = 30
  ): Observable<ExpensiveResource[]> {
    return this.api.get<ExpensiveResource[]>(
      `board/jobs/expensive-by-resource?metric=${metric}&window_days=${windowDays}`
    );
  }

  getExpensiveEvalsByResource(
    metric: 'time' | 'rss' | 'heap' | 'thunks' | 'fncalls' | 'alloc',
    windowDays = 30
  ): Observable<ExpensiveEval[]> {
    return this.api.get<ExpensiveEval[]>(
      `board/evals/expensive-by-resource?metric=${metric}&window_days=${windowDays}`
    );
  }

  getEvalFlakeGraph(evaluationId: string): Observable<FlakeGraphNode[]> {
    return this.api.get<FlakeGraphNode[]>(`evals/${evaluationId}/flake-graph`);
  }

  query(metric: string, granularity = 'hour', org?: string): Observable<MetricPoint[]> {
    let url = `metrics/query?metric=${encodeURIComponent(metric)}&granularity=${granularity}`;
    if (org) {
      url += `&org=${org}`;
    }
    return this.api.get<MetricPoint[]>(url);
  }
}
