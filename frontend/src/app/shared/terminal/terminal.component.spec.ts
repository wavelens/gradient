/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Subject } from 'rxjs';
import { vi } from 'vitest';
import type { TerminalClosed, TerminalSource } from './terminal-source';
import { TerminalComponent } from './terminal.component';

const terminals = vi.hoisted(() => [] as {
  write: ReturnType<typeof vi.fn>;
  typed: (data: string) => void;
  options: { disableStdin?: boolean };
}[]);

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    options: { disableStdin?: boolean } = {};
    write = vi.fn();
    typed: (data: string) => void = () => undefined;
    constructor() {
      terminals.push(this);
    }
    loadAddon(): void {}
    open(): void {}
    reset(): void {}
    dispose(): void {}
    onData(handler: (data: string) => void) {
      this.typed = handler;
      return { dispose: () => undefined };
    }
    onResize() {
      return { dispose: () => undefined };
    }
  },
}));

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit(): void {}
  },
}));

function fakeSource() {
  const output = new Subject<Uint8Array>();
  const closed = new Subject<TerminalClosed>();
  const source: TerminalSource = {
    output,
    closed,
    write: vi.fn(),
    resize: vi.fn(),
    dispose: vi.fn(),
  };
  return { source, output, closed };
}

describe('TerminalComponent', () => {
  beforeEach(() => {
    terminals.length = 0;
  });

  function render(source: TerminalSource) {
    const fixture = TestBed.createComponent(TerminalComponent);
    fixture.componentRef.setInput('source', source);
    fixture.detectChanges();
    TestBed.tick();
    return fixture;
  }

  it('writes the source output into the terminal', () => {
    const { source, output } = fakeSource();
    render(source);
    const bytes = new Uint8Array([104, 105]);
    output.next(bytes);
    expect(terminals[0].write).toHaveBeenCalledWith(bytes);
  });

  it('sends typed text to the source', () => {
    const { source } = fakeSource();
    render(source);
    terminals[0].typed('ls\r');
    expect(source.write).toHaveBeenCalledWith('ls\r');
  });

  it('prints the close message and stops input', () => {
    const { source, closed } = fakeSource();
    render(source);
    closed.next({ exitCode: null, message: 'the worker disconnected' });
    terminals[0].typed('x');
    const printed = terminals[0].write.mock.calls.map(([text]) => String(text)).join('');
    expect(printed).toContain('the worker disconnected');
    expect(source.write).not.toHaveBeenCalled();
    expect(terminals[0].options.disableStdin).toBe(true);
  });

  it('disposes the source it was given when it goes away', () => {
    const { source } = fakeSource();
    render(source).destroy();
    expect(source.dispose).toHaveBeenCalled();
  });
});
