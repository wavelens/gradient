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

  private base(org: string, proj: string): string {
    return `projects/${org}/${proj}/flake-inputs`;
  }

  list(org: string, proj: string): Observable<FlakeInputOverride[]> {
    return this.api.get<FlakeInputOverride[]>(this.base(org, proj));
  }

  create(org: string, proj: string, body: CreateFlakeInputOverrideBody): Observable<FlakeInputOverride> {
    return this.api.post<FlakeInputOverride>(this.base(org, proj), body);
  }

  get(org: string, proj: string, id: string): Observable<FlakeInputOverride> {
    return this.api.get<FlakeInputOverride>(`${this.base(org, proj)}/${id}`);
  }

  update(org: string, proj: string, id: string, body: UpdateFlakeInputOverrideBody): Observable<FlakeInputOverride> {
    return this.api.patch<FlakeInputOverride>(`${this.base(org, proj)}/${id}`, body);
  }

  delete(org: string, proj: string, id: string): Observable<boolean> {
    return this.api.delete<boolean>(`${this.base(org, proj)}/${id}`);
  }
}
