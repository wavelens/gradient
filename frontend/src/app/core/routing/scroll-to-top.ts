/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EnvironmentProviders, inject, provideEnvironmentInitializer } from '@angular/core';
import { DOCUMENT, ViewportScroller } from '@angular/common';
import { NavigationEnd, Router, Scroll } from '@angular/router';
import { filter } from 'rxjs/operators';

const RESTORE_TIMEOUT_MS = 5000;
const USER_SCROLL_EVENTS = ['wheel', 'touchstart', 'keydown', 'pointerdown'] as const;

function pathOf(event: Scroll): string {
  const end = event.routerEvent;
  const url = end instanceof NavigationEnd ? end.urlAfterRedirects : end.url;
  return url.split(/[?#]/)[0];
}

export function scrollTarget(event: Scroll, previousPath: string | null): [number, number] | null {
  if (event.position) return event.position;
  return pathOf(event) === previousPath ? null : [0, 0];
}

// Page content fills in after data load. Reapply the saved position while the page grows. Stop when the page reaches that position, when the user scrolls, or after a few seconds.
export function restoreScroll(position: [number, number], scroller: ViewportScroller, document: Document): () => void {
  const apply = () => scroller.scrollToPosition(position, { behavior: 'instant' });
  const reached = () => Math.abs(scroller.getScrollPosition()[1] - position[1]) < 2;
  apply();
  const view = document.defaultView;
  if (reached() || !view || typeof ResizeObserver === 'undefined') return () => undefined;

  const observer = new ResizeObserver(() => {
    apply();
    if (reached()) stop();
  });
  const timer = setTimeout(() => stop(), RESTORE_TIMEOUT_MS);
  const stop = () => {
    observer.disconnect();
    clearTimeout(timer);
    for (const name of USER_SCROLL_EVENTS) view.removeEventListener(name, stop, true);
  };
  observer.observe(document.body);
  for (const name of USER_SCROLL_EVENTS) view.addEventListener(name, stop, { capture: true, passive: true });
  return stop;
}

export function provideScrollToTopOnPageChange(): EnvironmentProviders {
  return provideEnvironmentInitializer(() => {
    const scroller = inject(ViewportScroller);
    const document = inject(DOCUMENT);
    let previousPath: string | null = null;
    let stopRestoring: () => void = () => undefined;
    scroller.setHistoryScrollRestoration('manual');
    inject(Router)
      .events.pipe(filter((event) => event instanceof Scroll))
      .subscribe((event) => {
        stopRestoring();
        const target = scrollTarget(event, previousPath);
        if (event.position) stopRestoring = restoreScroll(event.position, scroller, document);
        else if (target) scroller.scrollToPosition(target, { behavior: 'instant' });
        previousPath = pathOf(event);
      });
  });
}
