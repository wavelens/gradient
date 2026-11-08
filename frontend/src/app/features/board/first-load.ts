/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, signal } from '@angular/core';
import { EMPTY, MonoTypeOperatorFunction, catchError, defer, finalize, tap } from 'rxjs';

export interface FirstLoad {
  /// True until every tracked request of the first paint has settled.
  loading: Signal<boolean>;
  received: Signal<boolean>;
  error: Signal<unknown>;
  track<T>(): MonoTypeOperatorFunction<T>;
  refresh(): void;
  restart(): void;
}

/// A board page polls, so only its first fetch earns a spinner: later refreshes
/// replace what is already on screen. Piping every request of the first paint
/// through `track()` holds the spinner until all of them have answered, rather
/// than clearing it on whichever returns first.
export function firstLoad(): FirstLoad {
  const loading = signal(true);
  const received = signal(false);
  const error = signal<unknown>(null);
  let pending = 0;
  let reporting = true;
  let hasReceivedDataInCurrentLoadRound = false;
  let hasTrackedRequestFailedInCurrentLoadRound = false;

  return {
    loading: loading.asReadonly(),
    received: received.asReadonly(),
    error: error.asReadonly(),
    refresh(): void {
      error.set(null);
      reporting = true;
    },
    restart(): void {
      loading.set(true);
      received.set(false);
      hasReceivedDataInCurrentLoadRound = false;
      this.refresh();
    },
    track<T>(): MonoTypeOperatorFunction<T> {
      return (source) =>
        defer(() => {
          pending += 1;
          return source;
        }).pipe(
          tap(() => {
            hasReceivedDataInCurrentLoadRound = true;
            if (!loading()) received.set(true);
          }),
          catchError((failure: unknown) => {
            hasTrackedRequestFailedInCurrentLoadRound = true;
            if (reporting) error.set(failure);
            return EMPTY;
          }),
          finalize(() => {
            pending -= 1;
            if (pending === 0) {
              if (!reporting && !hasTrackedRequestFailedInCurrentLoadRound) error.set(null);
              hasTrackedRequestFailedInCurrentLoadRound = false;
              received.set(hasReceivedDataInCurrentLoadRound);
              loading.set(false);
              reporting = false;
            }
          })
        );
    },
  };
}
