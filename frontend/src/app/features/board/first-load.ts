/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, signal } from '@angular/core';
import { MonoTypeOperatorFunction, defer, finalize } from 'rxjs';

export interface FirstLoad {
  /// True until every tracked request of the first paint has settled.
  loading: Signal<boolean>;
  track<T>(): MonoTypeOperatorFunction<T>;
}

/// A board page polls, so only its first fetch earns a spinner: later refreshes
/// replace what is already on screen. Piping every request of the first paint
/// through `track()` holds the spinner until all of them have answered, rather
/// than clearing it on whichever returns first.
export function firstLoad(): FirstLoad {
  const loading = signal(true);
  let pending = 0;

  return {
    loading: loading.asReadonly(),
    track<T>(): MonoTypeOperatorFunction<T> {
      return (source) =>
        defer(() => {
          pending += 1;
          return source;
        }).pipe(
          finalize(() => {
            pending -= 1;
            if (pending === 0) {
              loading.set(false);
            }
          })
        );
    },
  };
}
