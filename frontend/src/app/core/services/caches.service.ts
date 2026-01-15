/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Cache } from '@core/models';

@Injectable({ providedIn: 'root' })
export class CachesService {
  private api = inject(ApiService);

  getCaches(): Observable<Cache[]> {
    return this.api.get<Cache[]>('caches');
  }

  getCache(cache: string): Observable<Cache> {
    return this.api.get<Cache>(`caches/${cache}`);
  }

  createCache(data: {
    name: string;
    display_name: string;
    description: string;
    priority: number;
  }): Observable<Cache> {
    return this.api.put<Cache>('caches', data);
  }

  updateCache(cache: string, data: Partial<Cache>): Observable<Cache> {
    return this.api.patch<Cache>(`caches/${cache}`, data);
  }

  deleteCache(cache: string): Observable<void> {
    return this.api.delete<void>(`caches/${cache}`);
  }

  activateCache(cache: string): Observable<void> {
    return this.api.post<void>(`caches/${cache}/active`);
  }

  deactivateCache(cache: string): Observable<void> {
    return this.api.delete<void>(`caches/${cache}/active`);
  }
}
