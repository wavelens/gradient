/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DOCUMENT, DestroyRef, Signal, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { filter, fromEvent } from 'rxjs';
import { BoardLiveService, type BoardLiveEvent } from '@core/services/board-live.service';

const RELOAD_THROTTLE_MS = 1500;

export function subscribeToBoardSocketWithReload(
  reload: () => void,
  onEvent: (event: BoardLiveEvent) => void = () => {},
): Signal<boolean | null> {
  const connected = signal<boolean | null>(null);
  const document = inject(DOCUMENT);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let missedReloadWhileHidden = false;
  const scheduleReload = () => {
    if (timer) return;
    timer = setTimeout(() => {
      timer = undefined;
      if (document.visibilityState === 'visible') reload();
      else missedReloadWhileHidden = true;
    }, RELOAD_THROTTLE_MS);
  };
  fromEvent(document, 'visibilitychange')
    .pipe(filter(() => missedReloadWhileHidden && document.visibilityState === 'visible'), takeUntilDestroyed())
    .subscribe(() => {
      missedReloadWhileHidden = false;
      reload();
    });
  inject(DestroyRef).onDestroy(() => clearTimeout(timer));
  const ignoreFirstLiveBoardEventUntilMs = Date.now() + RELOAD_THROTTLE_MS;
  let hasReceivedLiveEvent = false;

  inject(BoardLiveService)
    .connect((open) => {
      if (open && connected() === false) reload();
      connected.set(open);
    })
    .pipe(takeUntilDestroyed())
    .subscribe((event) => {
      onEvent(event);
      const isFirstEventDuringInitialLoad = !hasReceivedLiveEvent && Date.now() < ignoreFirstLiveBoardEventUntilMs;
      hasReceivedLiveEvent = true;
      if (!isFirstEventDuringInitialLoad) scheduleReload();
    });

  return connected.asReadonly();
}
