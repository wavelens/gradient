/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap } from '@angular/router';
import { oneOf, queryParamSignal } from './query-param-signal';

function setup(query: Record<string, string>) {
  const navigate = vi.fn().mockResolvedValue(true);
  TestBed.configureTestingModule({
    providers: [
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap(query) } } },
      { provide: Router, useValue: { navigate } },
    ],
  });
  const tab = TestBed.runInInjectionContext(() => queryParamSignal('tab', 'time', oneOf(['time', 'disk'] as const)));
  const days = TestBed.runInInjectionContext(() => queryParamSignal('days', 30, oneOf([7, 30, 90])));
  return { tab, days, navigate };
}

describe('queryParamSignal', () => {
  it('starts from the URL and from the default for a missing or unknown value', () => {
    const { tab, days } = setup({ tab: 'disk', days: '45' });
    expect(tab()).toBe('disk');
    expect(days()).toBe(30);
  });

  it('writes a change into the URL in place and drops the default from it', () => {
    const { tab, days, navigate } = setup({});
    days.set(90);
    expect(days()).toBe(90);
    expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({
      queryParams: { days: 90 }, queryParamsHandling: 'merge', replaceUrl: true,
    }));
    tab.set('time');
    expect(navigate).toHaveBeenLastCalledWith([], expect.objectContaining({ queryParams: { tab: null } }));
  });
});
