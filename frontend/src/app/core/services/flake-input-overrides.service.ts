/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  FlakeInputOverride,
  CreateFlakeInputOverrideBody,
  UpdateFlakeInputOverrideBody,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class FlakeInputOverridesService {
  private api = inject(ApiService);

  private base(project: string, proj: string): string {
    return `tasks/${project}/${proj}/flake-inputs`;
  }

  list(project: string, proj: string): Observable<FlakeInputOverride[]> {
    return this.api.get<FlakeInputOverride[]>(this.base(project, proj));
  }

  create(project: string, proj: string, body: CreateFlakeInputOverrideBody): Observable<FlakeInputOverride> {
    return this.api.post<FlakeInputOverride>(this.base(project, proj), body);
  }

  get(project: string, proj: string, id: string): Observable<FlakeInputOverride> {
    return this.api.get<FlakeInputOverride>(`${this.base(project, proj)}/${id}`);
  }

  update(project: string, proj: string, id: string, body: UpdateFlakeInputOverrideBody): Observable<FlakeInputOverride> {
    return this.api.patch<FlakeInputOverride>(`${this.base(project, proj)}/${id}`, body);
  }

  delete(project: string, proj: string, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`${this.base(project, proj)}/${id}`);
  }
}
