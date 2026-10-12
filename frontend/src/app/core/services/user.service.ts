/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import {
  GetUserSettingsResponse,
  SshKeyInfo,
  ApiKeyInfo,
  SessionInfo,
  Paginated,
  AuditLogEntry,
  InviteItem,
  PatchUserSettingsRequest,
  DeleteUserRequest,
  CreateApiKeyRequest,
  PatchApiKeyRequest,
  ApiKeyPermissionsResponse,
  UserSearchResult,
} from '@core/models';

@Injectable({ providedIn: 'root' })
export class UserService {
  private api = inject(ApiService);

  getUserSettings(): Observable<GetUserSettingsResponse> {
    return this.api.get<GetUserSettingsResponse>('user/settings');
  }

  updateUserSettings(data: PatchUserSettingsRequest): Observable<string> {
    return this.api.patch<string>('user/settings', data);
  }

  deleteUser(confirmation: DeleteUserRequest): Observable<string> {
    return this.api.delete<string>('user', confirmation);
  }

  getSshKeys(): Observable<SshKeyInfo[]> {
    return this.api.get<SshKeyInfo[]>('user/ssh-keys');
  }

  addSshKey(name: string, publicKey: string): Observable<SshKeyInfo> {
    return this.api.post<SshKeyInfo>('user/ssh-keys', { name, public_key: publicKey });
  }

  deleteSshKey(id: string): Observable<string> {
    return this.api.delete<string>(`user/ssh-keys/${id}`);
  }

  getApiKeys(): Observable<ApiKeyInfo[]> {
    return this.api.get<ApiKeyInfo[]>('user/keys');
  }

  createApiKey(
    name: string,
    expiresInDays?: number | null,
    permissions: string[] = ['viewProject'],
    project: string | null = null,
    cache: string | null = null,
    allowedIps: string[] = [],
  ): Observable<string> {
    const body: CreateApiKeyRequest = { name, permissions, project, cache, allowed_ips: allowedIps };
    if (expiresInDays !== null && expiresInDays !== undefined) {
      body.expires_in_days = expiresInDays;
    }
    return this.api.post<string>('user/keys', body);
  }

  updateApiKey(
    apiId: string,
    body: PatchApiKeyRequest,
  ): Observable<ApiKeyInfo> {
    return this.api.patch<ApiKeyInfo>(`user/keys/${apiId}`, body);
  }

  getApiKeyPermissions(): Observable<ApiKeyPermissionsResponse> {
    return this.api.get<ApiKeyPermissionsResponse>('user/keys/permissions');
  }

  deleteApiKey(name: string): Observable<string> {
    return this.api.delete<string>('user/keys', { name });
  }

  revokeApiKey(id: string): Observable<string> {
    return this.api.post<string>(`user/keys/${id}/revoke`, {});
  }

  getInvites(): Observable<InviteItem[]> {
    return this.api.get<InviteItem[]>('user/invites');
  }

  acceptInvite(token: string): Observable<string> {
    return this.api.post<string>('user/invites/accept', { token });
  }

  declineInvite(token: string): Observable<string> {
    return this.api.post<string>('user/invites/decline', { token });
  }

  getSessions(): Observable<SessionInfo[]> {
    return this.api.get<SessionInfo[]>('user/sessions');
  }

  revokeSession(id: string): Observable<string> {
    return this.api.delete<string>(`user/sessions/${id}`);
  }

  getAuditLog(page = 1, perPage = 50): Observable<Paginated<AuditLogEntry[]>> {
    return this.api.get<Paginated<AuditLogEntry[]>>(
      `user/audit-log?page=${page}&per_page=${perPage}`,
    );
  }

  searchUsers(query: string): Observable<UserSearchResult[]> {
    return this.api.get<UserSearchResult[]>(`user/search?q=${encodeURIComponent(query)}`);
  }
}
