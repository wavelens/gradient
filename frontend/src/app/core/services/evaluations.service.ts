/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Evaluation, EvaluationMessage } from '@core/models';

export interface DependencyNode {
  id: string;
  name: string;
  path: string;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface DependencyEdge {
  source: string;
  target: string;
}

export interface BuildGraph {
  root: string;
  nodes: DependencyNode[];
  edges: DependencyEdge[];
}

export interface BuildItem {
  id: string;
  name: string;          // derivation path
  status: string;        // BuildStatus as string
  has_artefacts: boolean;
  updated_at: string;
  build_time_ms: number | null;
}

export interface PaginatedBuilds {
  builds: BuildItem[];
  total: number;
  /** Builds in Building/Queued/Failed/Aborted/DependencyFailed state — all must be in memory for correct log streaming. */
  active_count: number;
}

export interface BuildProduct {
  file_type: string;
  name: string;
  path: string;
  size?: number;
}

@Injectable({ providedIn: 'root' })
export class EvaluationsService {
  private api = inject(ApiService);

  getEvaluation(id: string): Observable<Evaluation> {
    return this.api.get<Evaluation>(`evals/${id}`);
  }

  getEvaluationMessages(evalId: string): Observable<EvaluationMessage[]> {
    return this.api.get<EvaluationMessage[]>(`evals/${evalId}/messages`);
  }

  abortEvaluation(id: string): Observable<string> {
    return this.api.post<string>(`evals/${id}`, { method: 'abort' });
  }

  getBuilds(evaluationId: string, limit?: number, offset?: number): Observable<PaginatedBuilds> {
    const params: string[] = [];
    if (limit !== undefined) params.push(`limit=${limit}`);
    if (offset !== undefined) params.push(`offset=${offset}`);
    const query = params.length > 0 ? `?${params.join('&')}` : '';
    return this.api.get<PaginatedBuilds>(`evals/${evaluationId}/builds${query}`);
  }

  getBuildLog(buildId: string): Observable<string> {
    return this.api.get<string>(`builds/${buildId}/log`);
  }

  getBuildDependencies(buildId: string): Observable<DependencyNode[]> {
    return this.api.get<DependencyNode[]>(`builds/${buildId}/dependencies`);
  }

  getBuildGraph(buildId: string): Observable<BuildGraph> {
    return this.api.get<BuildGraph>(`builds/${buildId}/graph`);
  }

  getBuildDownloads(buildId: string): Observable<BuildProduct[]> {
    return this.api.get<BuildProduct[]>(`builds/${buildId}/downloads`);
  }

  getDownloadToken(buildId: string): Observable<string> {
    return this.api.get<string>(`builds/${buildId}/download-token`);
  }
}
