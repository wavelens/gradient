/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable, shareReplay } from 'rxjs';
import { ApiService } from './api.service';
import {
  GradientCapabilities,
  RuleDescription,
  DispatchedJobsResponse,
  PendingJobsResponse,
  AssignDecisionView,
  BoardWorker,
  WorkerLoad,
  ExpensiveBuild,
  MetricMeta,
  ScoringSummary,
  BoardCacheStats,
  BoardUpstreamCacheStats,
  BoardNetworkStats,
  BoardFleetPoint,
  BoardHealth,
  BoardStorage,
  DurationsHeatmap,
  TopProjectBuildTime,
  ExpensiveResource,
  ExpensiveEval,
  FlakeGraphNode,
  MetricPoint,
  DispatchedJobDetail,
} from '@core/models';

export interface Windowed { w5m: number | null; w1h: number | null; w24h: number | null; }

export interface WorkerContextView {
  architectures: string[];
  system_features: string[];
  capabilities: GradientCapabilities;
  cpu_count: number;
  cpu_core_score: number;
  ram_total_mb: number;
  ram_free_mb: number | null;
  ram_reserved_mb?: number;
  ram_reserved_unused_mb?: number | null;
  ram_reserved_for_waiting_build_mb?: number;
  cpu_usage_pct: number | null;
  disk_speed_mbps: number | null;
  upload_speed_mbps: number | null;
  download_speed_mbps: number | null;
}

export interface DerivationRef { build_id: string; drv_path: string; pname: string | null; }

export interface JobEstimateView {
  own_samples: number; cores: number; ram_mb: number | null; build_ms: number | null;
  output_nar_bytes: number | null; similar_builds: [string, number][];
}

export interface JobContextView {
  kind: 'Build' | 'Eval';
  architecture: string;
  missing_count: number | null;
  missing_nar_size: number | null;
  outputs_present: boolean;
  substitute_outputs?: number;
  project_work_share: number | null;
  rescore_count: number;
  queued_at: string;
  ready_at: string;
  dependency_count?: number;
  pname?: string | null;
  closure_size?: number | null;
  prefer_local_build?: boolean;
  is_fixed_output?: boolean;
  estimate?: JobEstimateView;
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
  cpu_core_score_mean?: number | null;
}

export interface AssignedJobDetail
  extends Omit<DispatchedJobDetail, 'score_breakdown' | 'worker_context' | 'job_context' | 'instance_context'> {
  score_breakdown: { rules: Record<string, number>; total: number };
  worker_context: WorkerContextView;
  job_context: JobContextView;
  instance_context: InstanceContextView | null;
}

@Injectable({ providedIn: 'root' })
export class BoardService {
  private api = inject(ApiService);
  private scoringRules$?: Observable<RuleDescription[]>;

  getAssignedJobs(): Observable<DispatchedJobsResponse> {
    return this.api.get<DispatchedJobsResponse>('board/jobs/dispatched');
  }

  getPendingJobs(): Observable<PendingJobsResponse> {
    return this.api.get<PendingJobsResponse>('board/jobs/pending');
  }

  /// Recent assignment decisions with every candidate's score, including the
  /// rejected/negative ones the scheduler passed over (superuser-only).
  getAssignmentDecisions(): Observable<AssignDecisionView[]> {
    return this.api.get<AssignDecisionView[]>('board/jobs/decisions');
  }

  getJob(id: string): Observable<AssignedJobDetail> {
    return this.api.get<AssignedJobDetail>(`board/jobs/${id}`);
  }

  getWorkers(): Observable<BoardWorker[]> {
    return this.api.get<BoardWorker[]>('board/workers');
  }

  getWorkerLoad(): Observable<WorkerLoad> {
    return this.api.get<WorkerLoad>('board/workers/load');
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

  getUpstreamCacheStats(windowHours = 24): Observable<BoardUpstreamCacheStats> {
    return this.api.get<BoardUpstreamCacheStats>(`board/cache/upstream-caches?window_hours=${windowHours}`);
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

  getStorage(windowHours = 6): Observable<BoardStorage> {
    return this.api.get<BoardStorage>(`board/storage?window_hours=${windowHours}`);
  }

  getDurationsHeatmap(windowHours = 24): Observable<DurationsHeatmap> {
    return this.api.get<DurationsHeatmap>(`board/durations/heatmap?window_hours=${windowHours}`);
  }

  getTopProjects(windowDays = 30): Observable<TopProjectBuildTime[]> {
    return this.api.get<TopProjectBuildTime[]>(`board/expensive/top-projects?window_days=${windowDays}`);
  }

  getExpensiveByResource(
    metric: 'ram' | 'cpu' | 'disk',
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

  query(metric: string, granularity = 'hour', project?: string): Observable<MetricPoint[]> {
    let url = `metrics/query?metric=${encodeURIComponent(metric)}&granularity=${granularity}`;
    if (project) {
      url += `&project=${project}`;
    }
    return this.api.get<MetricPoint[]>(url);
  }
}
