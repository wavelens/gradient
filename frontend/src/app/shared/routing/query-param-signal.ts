/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, inject, signal } from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';

export type QueryParamSignal<T> = Signal<T> & { set(value: T): void };

export function oneOf<T extends string | number>(values: readonly T[]): (raw: string) => T | null {
  return (raw) => values.find((v) => String(v) === raw) ?? null;
}

export function queryParamSignal<T extends string | number>(
  name: string,
  fallback: T,
  parse: (raw: string) => T | null,
): QueryParamSignal<T> {
  const route = inject(ActivatedRoute);
  const router = inject(Router);
  const raw = route.snapshot.queryParamMap.get(name);
  const state = signal<T>((raw === null ? null : parse(raw)) ?? fallback);
  return Object.assign(state.asReadonly(), {
    set(value: T): void {
      state.set(value);
      void router.navigate([], {
        relativeTo: route,
        queryParams: { [name]: value === fallback ? null : value },
        queryParamsHandling: 'merge',
        replaceUrl: true,
      });
    },
  });
}
