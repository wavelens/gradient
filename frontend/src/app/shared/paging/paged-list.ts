/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, Signal, computed, inject, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Observable, tap } from 'rxjs';
import { Paginated } from '@core/models';
import { NotifyService } from '@core/services/notify.service';

export interface PagedList<T> {
  readonly items: Signal<T[]>;
  readonly total: Signal<number>;
  readonly loading: Signal<boolean>;
  readonly loadingMore: Signal<boolean>;
  readonly error: Signal<unknown>;
  reload(): void;
  loadMore(): void;
  refreshFirstPageSilently(): Observable<unknown>;
}

export function pagedList<T>(noun: string, fetch: (page: number) => Observable<Paginated<T[]>>): PagedList<T> {
  const notify = inject(NotifyService);
  const destroyRef = inject(DestroyRef);
  const items = signal<T[]>([]);
  const total = signal(0);
  const fetching = signal(false);
  const loadingMore = signal(false);
  const error = signal<unknown>(null);
  let page = 1;
  let generation = 0;

  const request = (target: number, onNext: (result: Paginated<T[]>) => void, onError: (err: unknown) => void) => {
    const current = ++generation;
    fetch(target).pipe(takeUntilDestroyed(destroyRef)).subscribe({
      next: (result) => current === generation && onNext(result),
      error: (err: unknown) => current === generation && onError(err),
    });
  };

  return {
    items,
    total,
    loading: computed(() => fetching() && items().length === 0),
    loadingMore,
    error,
    reload: () => {
      fetching.set(true);
      loadingMore.set(false);
      error.set(null);
      request(1, (result) => {
        page = 1;
        items.set(result.items);
        total.set(result.total);
        fetching.set(false);
      }, (err) => {
        error.set(err);
        fetching.set(false);
      });
    },
    loadMore: () => {
      if (loadingMore() || fetching() || items().length >= total()) return;
      loadingMore.set(true);
      request(page + 1, (result) => {
        page = result.page;
        items.update((shown) => [...shown, ...result.items]);
        total.set(result.total);
        loadingMore.set(false);
      }, (err) => {
        notify.failure(`Couldn't load more ${noun}`, err);
        loadingMore.set(false);
      });
    },
    refreshFirstPageSilently: () => {
      if (page !== 1 || fetching() || loadingMore() || error() !== null) return EMPTY;
      const current = generation;
      return fetch(1).pipe(
        tap((result) => {
          if (current !== generation) return;
          items.set(result.items);
          total.set(result.total);
        }),
      );
    },
  };
}
