/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import { ApiService } from './api.service';
import { SearchHit } from '@core/models';

@Injectable({ providedIn: 'root' })
export class SearchService {
  private api = inject(ApiService);

  search(q: string, limit?: number): Observable<SearchHit[]> {
    const bound = limit === undefined ? '' : `&limit=${limit}`;
    return this.api.get<SearchHit[]>(`search?q=${encodeURIComponent(q)}${bound}`);
  }
}
