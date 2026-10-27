/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Observable, Subject, catchError, debounceTime, map, of, switchMap } from 'rxjs';
import { NameCheckState } from '@gradient/ui/ui';
import { slugify } from '@shared/text';

const NAME = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
const DEBOUNCE_MS = 400;
const BLOCKING: NameCheckState[] = ['checking', 'invalid', 'reserved', 'taken'];

export interface NameAvailability {
  state: Signal<NameCheckState>;
  blocksSubmit(): boolean;
  check(name: string): void;
  suggest(displayName: string): string | null;
  markEdited(): void;
  reset(): void;
}

export function nameAvailability(
  isAvailable: (name: string) => Observable<boolean>,
  reserved: readonly string[] = [],
): NameAvailability {
  const state = signal<NameCheckState>('idle');
  const pending = new Subject<string>();
  let edited = false;

  pending
    .pipe(
      debounceTime(DEBOUNCE_MS),
      switchMap((name) =>
        name
          ? isAvailable(name).pipe(
              map((available): NameCheckState => (available ? 'available' : 'taken')),
              catchError(() => of<NameCheckState>('idle')),
            )
          : EMPTY,
      ),
      takeUntilDestroyed(),
    )
    .subscribe((result) => state.set(result));

  const settle = (result: NameCheckState): void => {
    state.set(result);
    pending.next('');
  };

  const check = (name: string): void => {
    if (!name) return settle('idle');
    if (!NAME.test(name)) return settle('invalid');
    if (reserved.includes(name)) return settle('reserved');
    state.set('checking');
    pending.next(name);
  };

  return {
    state: state.asReadonly(),
    blocksSubmit: () => BLOCKING.includes(state()),
    check,
    suggest(displayName) {
      if (edited) return null;
      const name = slugify(displayName);
      check(name);
      return name;
    },
    markEdited() {
      edited = true;
    },
    reset() {
      edited = false;
      check('');
    },
  };
}
