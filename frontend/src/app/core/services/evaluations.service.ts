/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Evaluation } from '@core/models';

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
}

export interface BuildProduct {
  file_type: string;
  name: string;
  path: string;
}

@Injectable({ providedIn: 'root' })
export class EvaluationsService {
  private api = inject(ApiService);

  getEvaluation(id: string): Observable<Evaluation> {
    return this.api.get<Evaluation>(`evals/${id}`);
  }

  abortEvaluation(id: string): Observable<string> {
    return this.api.post<string>(`evals/${id}`, { method: 'abort' });
  }

  getBuilds(evaluationId: string): Observable<BuildItem[]> {
    return this.api.get<BuildItem[]>(`evals/${evaluationId}/builds`);
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
