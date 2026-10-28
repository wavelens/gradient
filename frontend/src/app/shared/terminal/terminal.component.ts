/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  afterNextRender,
  effect,
  inject,
  input,
  signal,
  viewChild,
} from '@angular/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import type { TerminalClosed, TerminalSource } from './terminal-source';

const REPLAY_WINDOW_MS = 1000;
// eslint-disable-next-line no-control-regex -- ESC introduces the reports
const TERMINAL_REPORT = /^\x1b\[[?>]?[\d;]*[Rcn]$/;

function closedLine({ exitCode, message }: TerminalClosed): string {
  const text = message ?? (exitCode === null ? 'the terminal closed' : `exited with code ${exitCode}`);
  return `\r\n\x1b[2m[${text}]\x1b[0m\r\n`;
}

// The terminal disposes every source it was given once that source is replaced or the terminal goes away.
@Component({
  selector: 'gr-terminal',
  standalone: true,
  template: '<div class="terminal"><div #screen class="screen"></div></div>',
  styleUrl: './terminal.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class TerminalComponent {
  source = input.required<TerminalSource>();

  private screen = viewChild.required<ElementRef<HTMLElement>>('screen');
  private terminal = signal<Terminal | null>(null);

  constructor() {
    const destroyRef = inject(DestroyRef);
    afterNextRender(() => {
      const terminal = new Terminal({ cursorBlink: true, fontSize: 13 });
      const fit = new FitAddon();
      terminal.loadAddon(fit);
      terminal.open(this.screen().nativeElement);
      fit.fit();
      const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(() => fit.fit());
      observer?.observe(this.screen().nativeElement);
      destroyRef.onDestroy(() => {
        observer?.disconnect();
        terminal.dispose();
      });
      this.terminal.set(terminal);
    });

    effect((onCleanup) => {
      const terminal = this.terminal();
      const source = this.source();
      if (!terminal) return;
      onCleanup(this.connect(terminal, source));
    });
  }

  private connect(terminal: Terminal, source: TerminalSource): () => void {
    let accepting = true;
    let replayEnds: number | null = null;
    let parsingReplay = 0;
    terminal.reset();
    terminal.options.disableStdin = false;
    // A new viewer first receives the recent history; xterm answers the queries
    // inside it again and those late answers would land as input.
    const typing = terminal.onData((data) => {
      if (!accepting || (parsingReplay > 0 && TERMINAL_REPORT.test(data))) return;
      source.write(data);
    });
    const resizing = terminal.onResize(({ cols, rows }) => source.resize(cols, rows));
    const output = source.output.subscribe((bytes) => {
      const now = Date.now();
      replayEnds ??= now + REPLAY_WINDOW_MS;
      if (now > replayEnds) {
        terminal.write(bytes);
        return;
      }
      parsingReplay++;
      terminal.write(bytes, () => parsingReplay--);
    });
    const closed = source.closed.subscribe((reason) => {
      accepting = false;
      terminal.options.disableStdin = true;
      terminal.write(closedLine(reason));
    });
    source.resize(terminal.cols, terminal.rows);
    return () => {
      typing.dispose();
      resizing.dispose();
      output.unsubscribe();
      closed.unsubscribe();
      source.dispose();
    };
  }
}
