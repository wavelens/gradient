/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { Observable } from 'rxjs';
import type { EventEnvelope } from '@core/models';
import { LiveService } from './live.service';

export type BoardLiveEvent = EventEnvelope<{
  project?: string;
  worker_id?: string;
  kind?: number;
  score?: number;
  build_id?: string | null;
  evaluation_id?: string;
  workers?: number;
  pending?: number;
  active?: number;
}>;

@Injectable({ providedIn: 'root' })
export class BoardLiveService {
  private live = inject(LiveService);

  connect(onLiveSocketConnectedChange?: (connected: boolean) => void): Observable<BoardLiveEvent> {
    return this.live.connect<BoardLiveEvent>('/board/live', onLiveSocketConnectedChange);
  }
}
