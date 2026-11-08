/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Observable, of, throwError } from 'rxjs';
import { Paginated } from '@core/models';
import { NotifyService } from '@core/services/notify.service';
import { RequestOptions } from '@core/services/api.service';
import { BACKGROUND } from '@core/interceptors/request-context';
import { pagedList } from './paged-list';

const page = (items: number[], current: number, total = 3): Paginated<number[]> => ({ items, total, page: current, per_page: 2 });

function setup(fetch: (n: number, options?: RequestOptions) => Observable<Paginated<number[]>>) {
  const failure = vi.fn();
  TestBed.configureTestingModule({ providers: [{ provide: NotifyService, useValue: { failure } }] });
  const list = TestBed.runInInjectionContext(() => pagedList('numbers', fetch));
  return { list, failure };
}

describe('pagedList', () => {
  it('appends the next page and stops at the total', () => {
    const fetch = vi.fn((n: number) => of(n === 1 ? page([1, 2], 1) : page([3], 2)));
    const { list } = setup(fetch);
    list.reload();
    expect(list.items()).toEqual([1, 2]);
    list.loadMore();
    expect(list.items()).toEqual([1, 2, 3]);
    list.loadMore();
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('starts over on reload', () => {
    const { list } = setup((n) => of(n === 1 ? page([1, 2], 1) : page([3], 2)));
    list.reload();
    list.loadMore();
    list.reload();
    expect(list.items()).toEqual([1, 2]);
  });

  it('keeps the shown items and reports a failed next page', () => {
    const { list, failure } = setup((n) => (n === 1 ? of(page([1, 2], 1)) : throwError(() => new Error('down'))));
    list.reload();
    list.loadMore();
    expect(list.items()).toEqual([1, 2]);
    expect(list.error()).toBeNull();
    expect(list.loadingMore()).toBe(false);
    expect(failure).toHaveBeenCalledWith("Couldn't load more numbers", expect.any(Error));
  });

  it('swaps in a fresh first page without a spinner, but leaves a longer list alone', () => {
    let first = page([1, 2], 1);
    const fetch = vi.fn((n: number) => of(n === 1 ? first : page([3], 2)));
    const { list } = setup(fetch);
    list.reload();
    first = page([9, 1], 1, 4);
    list.refreshFirstPageSilently().subscribe();
    expect(list.items()).toEqual([9, 1]);
    expect(list.total()).toBe(4);

    list.loadMore();
    fetch.mockClear();
    list.refreshFirstPageSilently().subscribe();
    expect(fetch).not.toHaveBeenCalled();
  });

  it('marks the silent refresh as a background request so a failure keeps the page', () => {
    const fetch = vi.fn((_n: number, _options?: RequestOptions) => of(page([1, 2], 1)));
    const { list } = setup(fetch);
    list.reload();
    expect(fetch.mock.calls[0][1]).toBeUndefined();
    list.refreshFirstPageSilently().subscribe();
    expect(fetch.mock.calls[1][1]?.context?.get(BACKGROUND)).toBe(true);
  });
});
