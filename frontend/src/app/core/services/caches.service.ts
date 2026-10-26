/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  Paginated,
  MemberItem,
  RoleListResponse_CacheRoleOwner,
  RoleResponse_CacheRoleOwner,
  Cache,
  CacheStatsResponse,
  TrafficKind,
  UpstreamCacheItem,
  UpstreamTestResponse,
  NarSummary,
  NarDetail,
  NarStats,
  PendingInvitationItem,
  SubscriptionRequestItem,
  MakeCacheRequest,
  PatchCacheRequest,
  CacheResponse,
  AddUpstreamRequest,
  PatchUpstreamRequest,
  CreateRoleRequest,
  PatchRoleRequest,
} from '@core/models';

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

  getCache(cache: string): Observable<CacheResponse> {
    return this.api.get<CacheResponse>(`caches/${cache}`);
  }

  getPublicCaches(): Observable<Cache[]> {
    return this.api.get<Cache[]>('caches/public');
  }

  createCache(data: MakeCacheRequest): Observable<string> {
    return this.api.put<string>('caches', data);
  }

  setCachePublic(cache: string): Observable<string> {
    return this.api.post<string>(`caches/${cache}/public`);
  }

  setCachePrivate(cache: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/public`);
  }

  updateCache(cache: string, data: PatchCacheRequest): Observable<string> {
    return this.api.patch<string>(`caches/${cache}`, data);
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

  getCacheStats(cache: string, traffic: TrafficKind = 'total'): Observable<CacheStatsResponse> {
    return this.api.get<CacheStatsResponse>(`caches/${cache}/stats?traffic=${traffic}`);
  }

  getUpstreamCaches(cache: string): Observable<UpstreamCacheItem[]> {
    return this.api.get<UpstreamCacheItem[]>(`caches/${cache}/upstream-caches`);
  }

  addInternalUpstream(cache: string, data: Omit<Extract<AddUpstreamRequest, { type: 'internal' }>, 'type'>): Observable<string> {
    return this.api.put<string>(`caches/${cache}/upstream-caches`, { type: 'internal', ...data });
  }

  addGradientProtoUpstream(cache: string, data: Omit<Extract<AddUpstreamRequest, { type: 'gradient_proto' }>, 'type'>): Observable<string> {
    return this.api.put<string>(`caches/${cache}/upstream-caches`, { type: 'gradient_proto', ...data });
  }

  addHttpUpstream(cache: string, data: Omit<Extract<AddUpstreamRequest, { type: 'http' }>, 'type'>): Observable<string> {
    return this.api.put<string>(`caches/${cache}/upstream-caches`, { type: 'http', ...data });
  }

  updateUpstream(cache: string, upstreamId: string, data: PatchUpstreamRequest): Observable<string> {
    return this.api.patch<string>(`caches/${cache}/upstream-caches/${upstreamId}`, data);
  }

  testUpstream(cache: string, upstreamId: string): Observable<UpstreamTestResponse> {
    return this.api.post<UpstreamTestResponse>(`caches/${cache}/upstream-caches/${upstreamId}/test`, {});
  }

  removeUpstream(cache: string, upstreamId: string): Observable<void> {
    return this.api.delete<void>(`caches/${cache}/upstream-caches/${upstreamId}`);
  }

  getCacheNars(cache: string, query: NarListQuery = {}): Observable<Paginated<NarSummary[]>> {
    const params = new URLSearchParams();
    for (const [k, v] of Object.entries(query)) {
      if (v !== undefined && v !== null && v !== '') params.set(k, String(v));
    }
    const qs = params.toString();
    const suffix = qs ? `?${qs}` : '';
    return this.api.get<Paginated<NarSummary[]>>(`caches/${cache}/nars${suffix}`);
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

  getInvitations(cache: string): Observable<PendingInvitationItem[]> {
    return this.api.get<PendingInvitationItem[]>(`caches/${cache}/invitations`);
  }

  revokeInvitation(cache: string, user: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/invitations`, { user });
  }

  getSubscriptionRequests(cache: string): Observable<SubscriptionRequestItem[]> {
    return this.api.get<SubscriptionRequestItem[]>(`caches/${cache}/subscription-requests`);
  }

  approveSubscriptionRequest(cache: string, project: string): Observable<string> {
    return this.api.post<string>(`caches/${cache}/subscription-requests/${project}`, {});
  }

  denySubscriptionRequest(cache: string, project: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/subscription-requests/${project}`);
  }

  getMembers(cache: string): Observable<MemberItem[]> {
    return this.api.get<MemberItem[]>(`caches/${cache}/members`);
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

  getRoles(cache: string): Observable<RoleListResponse_CacheRoleOwner> {
    return this.api.get<RoleListResponse_CacheRoleOwner>(`caches/${cache}/roles`);
  }

  createRole(cache: string, data: CreateRoleRequest): Observable<RoleResponse_CacheRoleOwner> {
    return this.api.post<RoleResponse_CacheRoleOwner>(`caches/${cache}/roles`, data);
  }

  updateRole(cache: string, roleId: string, data: PatchRoleRequest): Observable<RoleResponse_CacheRoleOwner> {
    return this.api.patch<RoleResponse_CacheRoleOwner>(`caches/${cache}/roles/${roleId}`, data);
  }

  deleteRole(cache: string, roleId: string): Observable<boolean> {
    return this.api.delete<boolean>(`caches/${cache}/roles/${roleId}`);
  }
}
