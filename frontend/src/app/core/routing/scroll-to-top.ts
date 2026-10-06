/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { EnvironmentProviders, inject, provideEnvironmentInitializer } from '@angular/core';
import { ViewportScroller } from '@angular/common';
import { NavigationEnd, Router, Scroll } from '@angular/router';
import { filter } from 'rxjs/operators';

function pathOf(event: Scroll): string {
  const end = event.routerEvent;
  const url = end instanceof NavigationEnd ? end.urlAfterRedirects : end.url;
  return url.split(/[?#]/)[0];
}

export function scrollTarget(event: Scroll, previousPath: string | null): [number, number] | null {
  if (event.position) return event.position;
  return pathOf(event) === previousPath ? null : [0, 0];
}

// Filters, tabs and log line links only change the query or fragment and keep their position.
export function provideScrollToTopOnPageChange(): EnvironmentProviders {
  return provideEnvironmentInitializer(() => {
    const scroller = inject(ViewportScroller);
    let previousPath: string | null = null;
    scroller.setHistoryScrollRestoration('manual');
    inject(Router)
      .events.pipe(filter((event) => event instanceof Scroll))
      .subscribe((event) => {
        const target = scrollTarget(event, previousPath);
        if (target) scroller.scrollToPosition(target, { behavior: 'instant' });
        previousPath = pathOf(event);
      });
  });
}
