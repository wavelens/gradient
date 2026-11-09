/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, inject, signal } from '@angular/core';
import { EMPTY, MonoTypeOperatorFunction, catchError, defer, finalize, tap } from 'rxjs';
import { NotifyService } from '@core/services/notify.service';

export const requestFailedText = "Couldn't load";

export interface FirstLoad {
  /// True until every tracked request of the first paint has settled.
  loading: Signal<boolean>;
  received: Signal<boolean>;
  error: Signal<unknown>;
  track<T>(): MonoTypeOperatorFunction<T>;
  refresh(): void;
  restart(): void;
}

/// Pages on the board poll. Spinners mark the first fetch only. Later updates replace on-screen content. Pass every request for the first render through `track()`. A spinner can stay until all requests settle. Page errors only replace the page, when no requests in that round get a response. Partial failures keep the part that loaded and show a toast.
export function firstLoad(): FirstLoad {
  const notify = inject(NotifyService);
  const loading = signal(true);
  const received = signal(false);
  const error = signal<unknown>(null);
  let pending = 0;
  let reporting = true;
  let hasReceivedDataSinceRestart = false;
  let hasReceivedDataThisLoadRound = false;
  let loadRoundFailure: unknown = null;

  function finishLoadRound(): void {
    if (reporting) {
      error.set(hasReceivedDataThisLoadRound ? null : loadRoundFailure);
      if (hasReceivedDataThisLoadRound && loadRoundFailure !== null) notify.failure("Couldn't load some items", loadRoundFailure);
    } else if (loadRoundFailure === null) {
      error.set(null);
    }
    hasReceivedDataThisLoadRound = false;
    loadRoundFailure = null;
    received.set(hasReceivedDataSinceRestart);
    loading.set(false);
    reporting = false;
  }

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
      hasReceivedDataSinceRestart = false;
      this.refresh();
    },
    track<T>(): MonoTypeOperatorFunction<T> {
      return (source) =>
        defer(() => {
          pending += 1;
          return source;
        }).pipe(
          tap(() => {
            hasReceivedDataSinceRestart = true;
            hasReceivedDataThisLoadRound = true;
            if (!loading()) received.set(true);
          }),
          catchError((failure: unknown) => {
            loadRoundFailure = failure;
            return EMPTY;
          }),
          finalize(() => {
            pending -= 1;
            if (pending === 0) finishLoadRound();
          })
        );
    },
  };
}
