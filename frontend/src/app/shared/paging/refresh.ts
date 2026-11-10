/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DOCUMENT, DestroyRef, inject } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Observable, catchError, filter, fromEvent, interval, merge, of, switchMap } from 'rxjs';

export function refreshImmediatelyThenOnIntervalAndOnTabVisibleWhileTabVisible(document: Document, millisecondsBetweenTicks: number): Observable<unknown> {
  return merge(of(0), interval(millisecondsBetweenTicks), fromEvent(document, 'visibilitychange')).pipe(
    filter(() => document.visibilityState === 'visible'),
  );
}

/// A timer and the return of the browser tab to view both start `fetch` again. A list in a background tab can catch up without a reload. A failed run does not clear the content on screen. Later ticks retry the run.
export function runReloadIntervalWhileTabVisible(fetch: () => Observable<unknown>, millisecondsBetweenVisibleReloads = 15_000): void {
  const document = inject(DOCUMENT);
  merge(interval(millisecondsBetweenVisibleReloads), fromEvent(document, 'visibilitychange'))
    .pipe(
      filter(() => document.visibilityState === 'visible'),
      switchMap(() => fetch().pipe(catchError(() => EMPTY))),
      takeUntilDestroyed(inject(DestroyRef)),
    )
    .subscribe();
}
