/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Cache, Paginated } from '@core/models';
import { CacheMemberItem, CacheRole, CacheRoleListResponse } from '@core/models/cache-permission.model';

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
  total_nar_bytes: number;
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

export interface NarSummary {
  hash: string;
  store_path: string;
  package: string;
  nar_size: number | null;
  file_size: number | null;
  created_at: string;
  last_fetched_at: string | null;
}

export interface NarListResponse {
  items: NarSummary[];
  total: number;
  page: number;
  per_page: number;
}

export interface NarDetail extends NarSummary {
  file_hash: string | null;
  nar_hash: string | null;
  references: string[];
  deriver: string | null;
  ca: string | null;
  fetch_count: number;
  signed: boolean;
}

export interface NarStats {
  total_nars: number;
  total_nar_size: number;
  total_file_size: number;
  last_uploaded_at: string | null;
  oldest_fetched_at: string | null;
}

export interface NarListQuery {
  hash?: string;
  package?: string;
  sort?: string;
  order?: string;
  page?: number;
  per_page?: number;
}

@Injectable({ providedIn: 'root' })
export class CachesService {
  private api = inject(ApiService);

  checkCacheNameAvailable(name: string): Observable<boolean> {
    return this.api.get<boolean>(`caches/available?name=${encodeURIComponent(name)}`);
  }

  getCaches(page = 1, perPage = 50): Observable<Paginated<Cache[]>> {
    return this.api.get<Paginated<Cache[]>>(`caches?page=${page}&per_page=${perPage}`);
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
    local_priority?: number | null;
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

  addGradientProtoUpstream(cache: string, data: {
    url: string;
    remote_cache: string;
    display_name: string;
    mode?: CacheSubscriptionMode;
    api_key?: string;
  }): Observable<string> {
    return this.api.put<string>(`caches/${cache}/upstreams`, { type: 'gradient_proto', ...data });
  }

  addHttpUpstream(cache: string, data: {
    display_name: string;
    url: string;
    public_key: string;
  }): Observable<string> {
    return this.api.put<string>(`caches/${cache}/upstreams`, { type: 'http', ...data });
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

  getCacheNars(cache: string, query: NarListQuery = {}): Observable<NarListResponse> {
    const params = new URLSearchParams();
    for (const [k, v] of Object.entries(query)) {
      if (v !== undefined && v !== null && v !== '') params.set(k, String(v));
    }
    const qs = params.toString();
    const suffix = qs ? `?${qs}` : '';
    return this.api.get<NarListResponse>(`caches/${cache}/nars${suffix}`);
  }

  getCacheNar(cache: string, hash: string): Observable<NarDetail> {
    return this.api.get<NarDetail>(`caches/${cache}/nars/${hash}`);
  }

  getCacheNarStats(cache: string): Observable<NarStats> {
    return this.api.get<NarStats>(`caches/${cache}/nars/stats`);
  }

  deleteCacheNar(cache: string, hash: string): Observable<void> {
    return this.api.delete<void>(`caches/${cache}/nars/${hash}`);
  }

  getMembers(cache: string): Observable<CacheMemberItem[]> {
    return this.api.get<CacheMemberItem[]>(`caches/${cache}/members`);
  }

  addMember(cache: string, user: string, role: string): Observable<string> {
    return this.api.post<string>(`caches/${cache}/members`, { user, role });
  }

  updateMember(cache: string, user: string, role: string): Observable<string> {
    return this.api.patch<string>(`caches/${cache}/members`, { user, role });
  }

  removeMember(cache: string, user: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/members`, { user });
  }

  getRoles(cache: string): Observable<CacheRoleListResponse> {
    return this.api.get<CacheRoleListResponse>(`caches/${cache}/roles`);
  }

  createRole(cache: string, data: { name: string; permissions: string[] }): Observable<CacheRole> {
    return this.api.post<CacheRole>(`caches/${cache}/roles`, data);
  }

  updateRole(cache: string, roleId: string, data: { name?: string; permissions?: string[] }): Observable<CacheRole> {
    return this.api.patch<CacheRole>(`caches/${cache}/roles/${roleId}`, data);
  }

  deleteRole(cache: string, roleId: string): Observable<boolean> {
    return this.api.delete<boolean>(`caches/${cache}/roles/${roleId}`);
  }
}
