/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable, shareReplay } from 'rxjs';
import { ApiService } from './api.service';
import type { CatalogEntry } from '@core/models';

@Injectable({ providedIn: 'root' })
export class EventsService {
  private api = inject(ApiService);

  readonly catalog$: Observable<CatalogEntry[]> = this.api
    .get<CatalogEntry[]>('events/catalog')
    .pipe(shareReplay(1));
}
