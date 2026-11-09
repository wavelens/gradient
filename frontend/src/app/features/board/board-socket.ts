/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, Signal, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { BoardLiveService, type BoardLiveEvent } from '@core/services/board-live.service';

const RELOAD_THROTTLE_MS = 1500;

export function subscribeToBoardSocketWithReload(
  reload: () => void,
  onEvent: (event: BoardLiveEvent) => void = () => {},
): Signal<boolean | null> {
  const connected = signal<boolean | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  const scheduleReload = () => {
    if (timer) return;
    timer = setTimeout(() => {
      timer = undefined;
      reload();
    }, RELOAD_THROTTLE_MS);
  };
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
