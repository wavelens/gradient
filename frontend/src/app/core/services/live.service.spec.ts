/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Subscription } from 'rxjs';
import { LiveService } from './live.service';
import { environment } from '@environments/environment';

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: FakeWebSocket[] = [];

  readyState = FakeWebSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  closed = false;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  emit(data: string): void {
    this.onmessage?.({ data });
  }

  close(): void {
    this.closed = true;
    this.readyState = FakeWebSocket.CLOSED;
  }
}

describe('LiveService', () => {
  let service: LiveService;
  let sub: Subscription | undefined;
  const realWs = globalThis.WebSocket;

  beforeEach(() => {
    FakeWebSocket.instances = [];
    (globalThis as unknown as { WebSocket: unknown }).WebSocket = FakeWebSocket;
    service = new LiveService();
  });

  afterEach(() => {
    sub?.unsubscribe();
    sub = undefined;
    (globalThis as unknown as { WebSocket: unknown }).WebSocket = realWs;
  });

  it('builds a ws(s) url from the page protocol/host and the api path', () => {
    sub = service.connect('/evals/abc/live').subscribe();
    const ws = FakeWebSocket.instances[0];
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    expect(ws.url).toBe(`${proto}://${location.host}${environment.apiUrl}/evals/abc/live`);
  });

  it('emits a parsed JSON frame to subscribers', () => {
    const frames: unknown[] = [];
    sub = service.connect('/board/cache/live').subscribe((f) => frames.push(f));
    FakeWebSocket.instances[0].emit(JSON.stringify({ type: 'cache_changed' }));
    expect(frames).toEqual([{ type: 'cache_changed' }]);
  });

  it('ignores malformed frames', () => {
    const frames: unknown[] = [];
    sub = service.connect('/board/cache/live').subscribe((f) => frames.push(f));
    const ws = FakeWebSocket.instances[0];
    ws.emit('not json {');
    ws.emit(JSON.stringify({ type: 'build_status_changed' }));
    expect(frames).toEqual([{ type: 'build_status_changed' }]);
  });

  it('closes the socket on unsubscribe', () => {
    sub = service.connect('/projects/o/p/live').subscribe();
    const ws = FakeWebSocket.instances[0];
    expect(ws.closed).toBe(false);
    sub.unsubscribe();
    sub = undefined;
    expect(ws.closed).toBe(true);
  });
});
