/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable } from '@angular/core';
import { Observable } from 'rxjs';
import { environment } from '@environments/environment';

export interface BoardLiveEvent {
  type: 'job_dispatched' | 'worker_connected' | 'worker_disconnected' | 'queue_depth';
  organization?: string;
  worker_id?: string;
  kind?: number;
  score?: number;
  build_id?: string | null;
  evaluation_id?: string;
  workers?: number;
  pending?: number;
  active?: number;
}

@Injectable({ providedIn: 'root' })
export class BoardLiveService {
  /// Live board events over a WebSocket. Auto-completes on close; resubscribe to
  /// reconnect. Errors surface to the subscriber so callers can retry.
  connect(): Observable<BoardLiveEvent> {
    return new Observable<BoardLiveEvent>((subscriber) => {
      const proto = location.protocol === 'https:' ? 'wss' : 'ws';
      const url = `${proto}://${location.host}${environment.apiUrl}/board/live`;
      const ws = new WebSocket(url);
      ws.onmessage = (e) => {
        try {
          subscriber.next(JSON.parse(e.data) as BoardLiveEvent);
        } catch {
          /* ignore malformed frames */
        }
      };
      ws.onerror = () => subscriber.error(new Error('board live socket error'));
      ws.onclose = () => subscriber.complete();
      return () => {
        if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
          ws.close();
        }
      };
    });
  }
}
