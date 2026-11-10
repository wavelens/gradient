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
const SESSION_REVOKED_CLOSE_CODE = 1008;

/// Build the `onLiveSocketConnectedChange` callback for `LiveService.connect` (or `BoardLiveService.connect`). It calls `resync` each time the socket opens again after a drop, never on the first open, so the page refetches what it missed while offline. It passes every change on to `onConnectedChange`, which can drive `gr-live-status`.
export function createResyncOnLiveSocketReconnectCallback(
  resync: () => void,
  onConnectedChange?: (connected: boolean) => void,
): (connected: boolean) => void {
  let dropped = false;
  return (connected) => {
    if (connected && dropped) resync();
    dropped = !connected;
    onConnectedChange?.(connected);
  };
}

@Injectable({ providedIn: 'root' })
export class LiveService {
  /// Subscribe to a live channel. The path is relative to the API route (for example, ``/evals/{id}/live``). Frames arrive as parsed JSON. The socket can reconnect with capped exponential backoff, so an open page can survive short disconnects. The Observable can complete on a socket close by the server with close code 1008 because a session or API key became invalid. Otherwise, only the caller can end the subscription through unsubscribe. Connection events pass true to ``onLiveSocketConnectedChange`` on each connection open and false on each disconnect. A page can set a stale flag and refetch content that the page missed after a reconnect.
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
        ws.onclose = (event) => {
          if (stopped) return;
          onLiveSocketConnectedChange?.(false);
          if (event.code === SESSION_REVOKED_CLOSE_CODE) {
            subscriber.complete();
            return;
          }
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
