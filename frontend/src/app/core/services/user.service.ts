/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  ApiKey,
  AuditLogEntry,
  PaginatedResponse,
  Session,
  UserSettings,
} from '@core/models';
import { PermissionDescriptor } from '@core/models/permission.model';

@Injectable({ providedIn: 'root' })
export class UserService {
  private api = inject(ApiService);

  getUserSettings(): Observable<UserSettings> {
    return this.api.get<UserSettings>('user/settings');
  }

  updateUserSettings(data: { username?: string; name?: string; email?: string }): Observable<string> {
    return this.api.patch<string>('user/settings', data);
  }

  deleteUser(confirmation: { password?: string; confirm_username?: string }): Observable<string> {
    return this.api.delete<string>('user', confirmation);
  }

  getApiKeys(): Observable<ApiKey[]> {
    return this.api.get<ApiKey[]>('user/keys');
  }

  createApiKey(
    name: string,
    expiresInDays?: number | null,
    permissions: string[] = ['viewOrg'],
    organization: string | null = null,
    cache: string | null = null,
  ): Observable<string> {
    const body: {
      name: string;
      expires_in_days?: number;
      permissions: string[];
      organization: string | null;
      cache: string | null;
    } = { name, permissions, organization, cache };
    if (expiresInDays !== null && expiresInDays !== undefined) {
      body.expires_in_days = expiresInDays;
    }
    return this.api.post<string>('user/keys', body);
  }

  updateApiKey(
    apiId: string,
    body: {
      name?: string;
      permissions?: string[];
      organization?: string | null;
      cache?: string | null;
    },
  ): Observable<ApiKey> {
    return this.api.patch<ApiKey>(`user/keys/${apiId}`, body);
  }

  getApiKeyPermissions(): Observable<{
    available_permissions: PermissionDescriptor[];
    availableCache: PermissionDescriptor[];
  }> {
    return this.api.get<{
      available_permissions: PermissionDescriptor[];
      availableCache: PermissionDescriptor[];
    }>('user/keys/permissions');
  }

  deleteApiKey(name: string): Observable<string> {
    return this.api.delete<string>('user/keys', { name });
  }

  revokeApiKey(id: string): Observable<string> {
    return this.api.post<string>(`user/keys/${id}/revoke`, {});
  }

  getSessions(): Observable<Session[]> {
    return this.api.get<Session[]>('user/sessions');
  }

  revokeSession(id: string): Observable<string> {
    return this.api.delete<string>(`user/sessions/${id}`);
  }

  getAuditLog(page = 1, perPage = 50): Observable<PaginatedResponse<AuditLogEntry[]>> {
    return this.api.get<PaginatedResponse<AuditLogEntry[]>>(
      `user/audit-log?page=${page}&per_page=${perPage}`,
    );
  }

  searchUsers(query: string): Observable<{ id: string; username: string; name: string }[]> {
    return this.api.get<{ id: string; username: string; name: string }[]>(`user/search?q=${encodeURIComponent(query)}`);
  }
}
