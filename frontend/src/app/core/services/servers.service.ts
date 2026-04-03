/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { Server, Paginated } from '@core/models';

@Injectable({ providedIn: 'root' })
export class ServersService {
  private api = inject(ApiService);

  getServers(org: string, page = 1, perPage = 50): Observable<Paginated<{ id: string; name: string }[]>> {
    return this.api.get<Paginated<{ id: string; name: string }[]>>(`servers/${org}?page=${page}&per_page=${perPage}`);
  }

  getServer(org: string, server: string): Observable<Server> {
    return this.api.get<Server>(`servers/${org}/${server}`);
  }

  createServer(org: string, data: {
    name: string;
    display_name: string;
    host: string;
    port: number;
    username: string;
    architectures: string[];
    features: string[];
    max_concurrent_builds: number;
  }): Observable<string> {
    return this.api.put<string>(`servers/${org}`, data);
  }

  deleteServer(org: string, server: string): Observable<string> {
    return this.api.delete<string>(`servers/${org}/${server}`);
  }

  patchServer(org: string, server: string, data: {
    display_name?: string;
    host?: string;
    port?: number;
    username?: string;
    architectures?: string[];
    features?: string[];
    max_concurrent_builds?: number;
  }): Observable<string> {
    return this.api.patch<string>(`servers/${org}/${server}`, data);
  }

  checkConnection(org: string, server: string): Observable<string> {
    return this.api.post<string>(`servers/${org}/${server}/check-connection`, {});
  }
}
