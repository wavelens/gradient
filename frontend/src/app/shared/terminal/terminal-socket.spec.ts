/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { environment } from '@environments/environment';
import { terminalSocket } from './terminal-socket';
import type { TerminalClosed, TerminalSource } from './terminal-source';

class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  static instances: FakeWebSocket[] = [];

  readyState = FakeWebSocket.CONNECTING;
  binaryType = 'blob';
  sent: unknown[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  open(): void {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  emit(data: unknown): void {
    this.onmessage?.({ data });
  }

  send(data: unknown): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }
}

describe('terminalSocket', () => {
  const realWs = globalThis.WebSocket;
  let source: TerminalSource;

  beforeEach(() => {
    FakeWebSocket.instances = [];
    (globalThis as unknown as { WebSocket: unknown }).WebSocket = FakeWebSocket;
    source = terminalSocket('/exec-jobs/j1/console');
  });

  afterEach(() => {
    source.dispose();
    (globalThis as unknown as { WebSocket: unknown }).WebSocket = realWs;
  });

  const socket = (): FakeWebSocket => FakeWebSocket.instances[0];

  it('opens the api path as a binary websocket', () => {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    expect(socket().url).toBe(`${proto}://${location.host}${environment.apiUrl}/exec-jobs/j1/console`);
    expect(socket().binaryType).toBe('arraybuffer');
  });

  it('passes binary frames on as output', () => {
    const output: Uint8Array[] = [];
    source.output.subscribe((bytes) => output.push(bytes));
    socket().open();
    socket().emit(new Uint8Array([104, 105]).buffer);
    expect(output.map((bytes) => Array.from(bytes))).toEqual([[104, 105]]);
  });

  it('sends a resize as a JSON text frame, also when asked before the socket opened', () => {
    source.resize(120, 40);
    expect(socket().sent).toEqual([]);
    socket().open();
    expect(socket().sent).toEqual([JSON.stringify({ type: 'resize', cols: 120, rows: 40 })]);
  });

  it('sends typed text as UTF-8 bytes', () => {
    socket().open();
    source.write('ä');
    expect(Array.from(socket().sent[0] as Uint8Array)).toEqual([0xc3, 0xa4]);
  });

  it('reports the closed frame once, without a lost connection after it', () => {
    const closed: TerminalClosed[] = [];
    source.closed.subscribe((c) => closed.push(c));
    socket().open();
    socket().emit(JSON.stringify({ type: 'closed', exit_code: 0, message: null }));
    socket().close();
    expect(closed).toEqual([{ exitCode: 0, message: null }]);
  });

  it('reports a close without the closed frame as a lost connection', () => {
    const closed: TerminalClosed[] = [];
    source.closed.subscribe((c) => closed.push(c));
    socket().open();
    socket().close();
    expect(closed).toEqual([{ exitCode: null, message: 'the connection was lost' }]);
    expect(FakeWebSocket.instances).toHaveLength(1);
  });

  it('reports nothing after it was disposed', () => {
    const closed: TerminalClosed[] = [];
    source.closed.subscribe((c) => closed.push(c));
    socket().open();
    source.dispose();
    expect(closed).toEqual([]);
    expect(socket().readyState).toBe(FakeWebSocket.CLOSED);
  });
});
