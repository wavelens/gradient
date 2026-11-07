/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { NavigationEnd, Scroll } from '@angular/router';
import type { ViewportScroller } from '@angular/common';
import { restoreScroll, scrollTarget } from './scroll-to-top';

function arrival(url: string, savedPosition: [number, number] | null = null): Scroll {
  return new Scroll(new NavigationEnd(1, url, url), savedPosition, null);
}

describe('scrollTarget', () => {
  it('starts a new page at the top', () => {
    expect(scrollTarget(arrival('/projects/b'), '/projects/a')).toEqual([0, 0]);
  });

  it('keeps the position when only the query or fragment changes', () => {
    expect(scrollTarget(arrival('/caches/c/nars?q=foo'), '/caches/c/nars')).toBeNull();
    expect(scrollTarget(arrival('/evaluations/e/log#L42'), '/evaluations/e/log')).toBeNull();
  });

  it('returns to the saved position on back and forward', () => {
    expect(scrollTarget(arrival('/projects/a', [0, 640]), '/projects/b')).toEqual([0, 640]);
  });
});

describe('restoreScroll', () => {
  it('applies the saved position again once the loaded page is tall enough, then stops', () => {
    let grow: () => void = () => undefined;
    vi.stubGlobal('ResizeObserver', class {
      constructor(callback: () => void) { grow = callback; }
      observe(): void { /* the test drives growth */ }
      disconnect(): void { grow = () => undefined; }
    });
    let pageHeight = 300;
    let y = 0;
    const scroller = {
      scrollToPosition: vi.fn(([, top]: [number, number]) => { y = Math.min(top, pageHeight); }),
      getScrollPosition: () => [0, y] as [number, number],
    } as unknown as ViewportScroller;

    restoreScroll([0, 900], scroller, document);
    expect(y).toBe(300);
    pageHeight = 2000;
    grow();
    expect(y).toBe(900);
    grow();
    expect(scroller.scrollToPosition).toHaveBeenCalledTimes(2);
    vi.unstubAllGlobals();
  });
});
