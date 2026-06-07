/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable } from '@angular/core';
import { Observable } from 'rxjs';
import { environment } from '@environments/environment';

export interface LiveEvent {
  type:
    | 'evaluation_status_changed'
    | 'build_status_changed'
    | 'cache_changed'
    | string;
  project?: string | null;
  evaluation_id?: string;
  build_id?: string;
  status?: number;
}

const MAX_BACKOFF_MS = 15000;

@Injectable({ providedIn: 'root' })
export class LiveService {
  /// Subscribe to a per-resource live channel (path is relative to the API
  /// root, e.g. `/evals/{id}/live`). Frames are parsed JSON. The socket
  /// reconnects with capped exponential backoff so a long-open page survives
  /// transient drops; the observable completes only when the caller
  /// unsubscribes.
  connect<T = LiveEvent>(path: string): Observable<T> {
    return new Observable<T>((subscriber) => {
      const proto = location.protocol === 'https:' ? 'wss' : 'ws';
      const url = `${proto}://${location.host}${environment.apiUrl}${path}`;
      let ws: WebSocket | null = null;
      let stopped = false;
      let attempt = 0;
      let timer: ReturnType<typeof setTimeout> | undefined;

      const open = () => {
        ws = new WebSocket(url);
        ws.onopen = () => (attempt = 0);
        ws.onmessage = (e) => {
          try {
            subscriber.next(JSON.parse(e.data) as T);
          } catch {
            /* ignore malformed frames */
          }
        };
        ws.onerror = () => ws?.close();
        ws.onclose = () => {
          if (stopped) return;
          timer = setTimeout(open, Math.min(1000 * 2 ** attempt++, MAX_BACKOFF_MS));
        };
      };
      open();

      return () => {
        stopped = true;
        if (timer) clearTimeout(timer);
        if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
          ws.close();
        }
      };
    });
  }
}
