/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, Injectable, inject, signal } from '@angular/core';

const TICK_MS = 1000;

/// All parts of the page share one clock with a tick each second. Templates read that clock instead of Date.now(). Relative time and durations in progress keep the same value across a pass of change detection and its dev-mode check.
@Injectable({ providedIn: 'root' })
export class CurrentTimeSignal {
  private readonly now = signal(Date.now());
  readonly currentTimeMilliseconds = this.now.asReadonly();

  constructor() {
    const timer = setInterval(() => this.now.set(Date.now()), TICK_MS);
    inject(DestroyRef).onDestroy(() => clearInterval(timer));
  }
}
