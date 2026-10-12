/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  DeletedResponse,
  FlakeInputOverrideOut,
  CreateFlakeInputOverrideRequest,
  UpdateFlakeInputOverrideRequest,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class FlakeInputOverridesService {
  private api = inject(ApiService);

  private base(project: string, proj: string): string {
    return `tasks/${project}/${proj}/flake-inputs`;
  }

  list(project: string, proj: string): Observable<FlakeInputOverrideOut[]> {
    return this.api.get<FlakeInputOverrideOut[]>(this.base(project, proj));
  }

  create(project: string, proj: string, body: CreateFlakeInputOverrideRequest): Observable<FlakeInputOverrideOut> {
    return this.api.post<FlakeInputOverrideOut>(this.base(project, proj), body);
  }

  get(project: string, proj: string, id: string): Observable<FlakeInputOverrideOut> {
    return this.api.get<FlakeInputOverrideOut>(`${this.base(project, proj)}/${id}`);
  }

  update(project: string, proj: string, id: string, body: UpdateFlakeInputOverrideRequest): Observable<FlakeInputOverrideOut> {
    return this.api.patch<FlakeInputOverrideOut>(`${this.base(project, proj)}/${id}`, body);
  }

  delete(project: string, proj: string, id: string): Observable<DeletedResponse> {
    return this.api.delete<DeletedResponse>(`${this.base(project, proj)}/${id}`);
  }
}
