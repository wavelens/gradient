/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable, tap } from 'rxjs';
import { ApiService } from './api.service';
import { AuthService } from './auth.service';
import {
  CachePublicKeyHistory,
  Paginated,
  RoleListResponse_CacheRoleOwner,
  RoleResponse_CacheRoleOwner,
  Cache,
  CreatedEntityResponse,
  CacheStatsResponse,
  TrafficKind,
  UpstreamCacheItem,
  UpstreamTestResponse,
  NarSummary,
  NarDetail,
  CachedPathPinStateChangeCount,
  NarStats,
  PendingInvitationItem,
  SubscriptionRequestItem,
  CacheSubscriptionMode,
  CacheSubscribersResponse,
  MakeCacheRequest,
  PatchCacheRequest,
  CacheResponse,
  AddUpstreamRequest,
  PatchUpstreamRequest,
  CreateRoleRequest,
  PatchRoleRequest,
  PersonWithAccessToProjectOrCache,
  AddMemberRequest,
} from '@core/models';

export interface NarListQuery {
  hash?: string;
  package?: string;
  sort?: string;
  order?: string;
  pinned?: boolean;
  page?: number;
  per_page?: number;
}

@Injectable({ providedIn: 'root' })
export class CachesService {
  private api = inject(ApiService);
  private auth = inject(AuthService);

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

  createCache(data: MakeCacheRequest): Observable<CreatedEntityResponse> {
    return this.api.put<CreatedEntityResponse>('caches', data).pipe(tap(() => this.auth.reloadUser()));
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

  rotateCacheKeyAndReturnNewPublicKey(cache: string): Observable<string> {
    return this.api.post<string>(`caches/${cache}/key/rotate`);
  }

  getPublicKeysWithDates(cache: string): Observable<CachePublicKeyHistory> {
    return this.api.get<CachePublicKeyHistory>(`caches/${cache}/keys`);
  }

  deleteCache(cache: string): Observable<void> {
    return this.api.delete<void>(`caches/${cache}`).pipe(tap(() => this.auth.reloadUser()));
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

  pinCacheNar(cache: string, hash: string, includeReferencedPaths: boolean): Observable<CachedPathPinStateChangeCount> {
    return this.api.post<CachedPathPinStateChangeCount>(
      `caches/${cache}/nars/${hash}/pin?include_referenced_paths=${includeReferencedPaths}`,
      {},
    );
  }

  unpinCacheNar(cache: string, hash: string, includeReferencedPaths: boolean): Observable<CachedPathPinStateChangeCount> {
    return this.api.delete<CachedPathPinStateChangeCount>(
      `caches/${cache}/nars/${hash}/pin?include_referenced_paths=${includeReferencedPaths}`,
    );
  }

  getInvitations(cache: string): Observable<PendingInvitationItem[]> {
    return this.api.get<PendingInvitationItem[]>(`caches/${cache}/invitations`);
  }

  revokeInvitation(cache: string, user: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/invitations`, { user });
  }

  getSubscribers(cache: string): Observable<CacheSubscribersResponse> {
    return this.api.get<CacheSubscribersResponse>(`caches/${cache}/subscribers`);
  }

  getSubscriptionRequests(cache: string): Observable<SubscriptionRequestItem[]> {
    return this.api.get<SubscriptionRequestItem[]>(`caches/${cache}/subscription-requests`);
  }

  approveSubscriptionRequest(cache: string, project: string, mode?: CacheSubscriptionMode): Observable<string> {
    return this.api.post<string>(`caches/${cache}/subscription-requests/${project}`, mode ? { mode } : {});
  }

  denySubscriptionRequest(cache: string, project: string): Observable<string> {
    return this.api.delete<string>(`caches/${cache}/subscription-requests/${project}`);
  }

  getAccess(cache: string): Observable<PersonWithAccessToProjectOrCache[]> {
    return this.api.get<PersonWithAccessToProjectOrCache[]>(`caches/${cache}/access`);
  }

  addMember(cache: string, user: string, role: string, addNow: boolean): Observable<string> {
    const request: AddMemberRequest = { user, role, add_as_member_immediately: addNow };
    return this.api.post<string>(`caches/${cache}/members`, request);
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
