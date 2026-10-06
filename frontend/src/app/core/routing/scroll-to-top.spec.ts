/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { NavigationEnd, Scroll } from '@angular/router';
import { scrollTarget } from './scroll-to-top';

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
