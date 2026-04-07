/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Cache } from '@core/models';

export type CacheSubscriptionMode = 'ReadWrite' | 'ReadOnly' | 'WriteOnly';

export interface UpstreamCache {
  id: string;
  display_name: string;
  mode: CacheSubscriptionMode;
  upstream_cache_id: string | null;
  url: string | null;
  public_key: string | null;
}

export interface CacheMetricPoint {
  time: string;
  bytes: number;
  requests: number;
}

export interface StorageMetricPoint {
  time: string;
  packages: number;
  bytes: number;
}

export interface CacheStats {
  total_bytes: number;
  total_packages: number;
  storage_minutes: StorageMetricPoint[];
  storage_hours: StorageMetricPoint[];
  storage_days: StorageMetricPoint[];
  storage_weeks: StorageMetricPoint[];
  minutes: CacheMetricPoint[];
  hours: CacheMetricPoint[];
  days: CacheMetricPoint[];
  weeks: CacheMetricPoint[];
}

@Injectable({ providedIn: 'root' })
export class CachesService {
  private api = inject(ApiService);

  checkCacheNameAvailable(name: string): Observable<boolean> {
    return this.api.get<boolean>(`caches/available?name=${encodeURIComponent(name)}`);
  }

  getCaches(): Observable<Cache[]> {
    return this.api.get<Cache[]>('caches');
  }

  getCache(cache: string): Observable<Cache> {
    return this.api.get<Cache>(`caches/${cache}`);
  }

  getPublicCaches(): Observable<Cache[]> {
    return this.api.get<Cache[]>('caches/public');
  }

  createCache(data: {
    name: string;
    display_name: string;
    description: string;
    priority: number;
    public?: boolean;
  }): Observable<Cache> {
    return this.api.put<Cache>('caches', data);
  }

  setCachePublic(cache: string): Observable<string> {
    return this.api.post<string>(`caches/${cache}/public`);
  }

  setCachePrivate(cache: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/public`);
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

  getCacheStats(cache: string): Observable<CacheStats> {
    return this.api.get<CacheStats>(`caches/${cache}/stats`);
  }

  getCacheUpstreams(cache: string): Observable<UpstreamCache[]> {
    return this.api.get<UpstreamCache[]>(`caches/${cache}/upstreams`);
  }

  addInternalUpstream(cache: string, data: {
    cache_name: string;
    display_name?: string;
    mode?: CacheSubscriptionMode;
  }): Observable<string> {
    return this.api.put<string>(`caches/${cache}/upstreams`, { type: 'internal', ...data });
  }

  addExternalUpstream(cache: string, data: {
    display_name: string;
    url: string;
    public_key: string;
  }): Observable<string> {
    return this.api.put<string>(`caches/${cache}/upstreams`, { type: 'external', ...data });
  }

  updateUpstream(cache: string, upstreamId: string, data: {
    display_name?: string;
    mode?: CacheSubscriptionMode;
    url?: string;
    public_key?: string;
  }): Observable<string> {
    return this.api.patch<string>(`caches/${cache}/upstreams/${upstreamId}`, data);
  }

  removeUpstream(cache: string, upstreamId: string): Observable<void> {
    return this.api.delete<void>(`caches/${cache}/upstreams/${upstreamId}`);
  }
}
