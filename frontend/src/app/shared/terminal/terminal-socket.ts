/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ReplaySubject, Subject } from 'rxjs';
import { environment } from '@environments/environment';
import type { TerminalClosed, TerminalSource } from './terminal-source';

interface ClosedFrame {
  type: 'closed';
  exit_code: number | null;
  message: string | null;
}

const CONNECTION_LOST: TerminalClosed = { exitCode: null, message: 'the connection was lost' };

function socketUrl(path: string): string {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  return `${proto}://${location.host}${environment.apiUrl}${path}`;
}

function parseClosed(text: string): TerminalClosed | null {
  try {
    const frame = JSON.parse(text) as ClosedFrame;
    return frame.type === 'closed' ? { exitCode: frame.exit_code, message: frame.message } : null;
  } catch {
    return null;
  }
}

export function terminalSocket(path: string): TerminalSource {
  const output = new Subject<Uint8Array>();
  const closed = new ReplaySubject<TerminalClosed>(1);
  const encoder = new TextEncoder();
  const ws = new WebSocket(socketUrl(path));
  ws.binaryType = 'arraybuffer';
  let pending: (string | Uint8Array<ArrayBuffer>)[] = [];
  let ended = false;

  const end = (reason: TerminalClosed | null) => {
    if (ended) return;
    ended = true;
    pending = [];
    if (reason) closed.next(reason);
    closed.complete();
    output.complete();
  };

  const send = (frame: string | Uint8Array<ArrayBuffer>) => {
    if (ended) return;
    if (ws.readyState === WebSocket.OPEN) ws.send(frame);
    else pending.push(frame);
  };

  ws.onopen = () => {
    pending.forEach((frame) => ws.send(frame));
    pending = [];
  };
  ws.onmessage = (event: MessageEvent) => {
    if (event.data instanceof ArrayBuffer) {
      output.next(new Uint8Array(event.data));
      return;
    }
    const reason = typeof event.data === 'string' ? parseClosed(event.data) : null;
    if (reason) end(reason);
  };
  ws.onerror = () => ws.close();
  ws.onclose = () => end(CONNECTION_LOST);

  return {
    output: output.asObservable(),
    closed: closed.asObservable(),
    write: (data) => send(encoder.encode(data)),
    resize: (cols, rows) => send(JSON.stringify({ type: 'resize', cols, rows })),
    dispose: () => {
      end(null);
      ws.close();
    },
  };
}
