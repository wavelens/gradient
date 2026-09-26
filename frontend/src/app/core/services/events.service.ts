/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable, shareReplay } from 'rxjs';
import { ApiService } from './api.service';
import type { EventCatalogEntry } from '@core/models';

@Injectable({ providedIn: 'root' })
export class EventsService {
  private api = inject(ApiService);

  readonly catalog$: Observable<EventCatalogEntry[]> = this.api
    .get<EventCatalogEntry[]>('events/catalog')
    .pipe(shareReplay(1));
}
