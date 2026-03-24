/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { UserSettings, ApiKey } from '@core/models';

@Injectable({ providedIn: 'root' })
export class UserService {
  private api = inject(ApiService);

  getUserSettings(): Observable<UserSettings> {
    return this.api.get<UserSettings>('user/settings');
  }

  updateUserSettings(data: { username?: string; name?: string; email?: string }): Observable<string> {
    return this.api.patch<string>('user/settings', data);
  }

  deleteUser(): Observable<string> {
    return this.api.delete<string>('user');
  }

  getApiKeys(): Observable<ApiKey[]> {
    return this.api.get<ApiKey[]>('user/keys');
  }

  createApiKey(name: string): Observable<string> {
    return this.api.post<string>('user/keys', { name });
  }

  deleteApiKey(name: string): Observable<string> {
    return this.api.delete<string>('user/keys', { name });
  }
}
