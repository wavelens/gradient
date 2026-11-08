/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable } from '@angular/core';
import { Observable } from 'rxjs';
import { environment } from '@environments/environment';
import type { EventEnvelope, BuildProgressPhase, EvaluationProgress } from '@core/models';

export type LiveEvent = EventEnvelope<{
  task?: string | null;
  evaluation_id?: string;
  build_id?: string;
  derivation_build?: string;
  status?: number;
  phase?: BuildProgressPhase;
  bytes_done?: number;
  bytes_total?: number | null;
  paths_done?: number;
  paths_total?: number | null;
  progress?: EvaluationProgress;
}>;

const MAX_BACKOFF_MS = 15000;

@Injectable({ providedIn: 'root' })
export class LiveService {
  /// Subscribe to a live channel. The path is relative to the API root, e.g. `/evals/{id}/live`. Frames arrive as parsed JSON. The socket can reconnect with capped exponential backoff, so an open page can survive short disconnects. Only the caller can complete the observable through unsubscribe. Connection events pass true to `onLiveSocketConnectedChange` on every open and false on every disconnect. A page can set a stale flag and refetch missed content after reconnect.
  connect<T = LiveEvent>(path: string, onLiveSocketConnectedChange?: (connected: boolean) => void): Observable<T> {
    return new Observable<T>((subscriber) => {
      const proto = location.protocol === 'https:' ? 'wss' : 'ws';
      const url = `${proto}://${location.host}${environment.apiUrl}${path}`;
      let ws: WebSocket | null = null;
      let stopped = false;
      let attempt = 0;
      let timer: ReturnType<typeof setTimeout> | undefined;

      const open = () => {
        ws = new WebSocket(url);
        ws.onopen = () => {
          attempt = 0;
          onLiveSocketConnectedChange?.(true);
        };
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
          onLiveSocketConnectedChange?.(false);
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
