/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { Observable } from 'rxjs';

export interface TerminalClosed {
  exitCode: number | null;
  message: string | null;
}

export interface TerminalSource {
  output: Observable<Uint8Array>;
  closed: Observable<TerminalClosed>;
  write(data: string): void;
  resize(cols: number, rows: number): void;
  dispose(): void;
}
