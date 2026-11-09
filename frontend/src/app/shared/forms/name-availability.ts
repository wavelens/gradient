/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Signal, signal } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { EMPTY, Observable, Subject, catchError, map, of, switchMap, timer } from 'rxjs';
import { NameCheckState } from '@gradient/ui/ui';
import { slugify } from '@shared/text';

const NAME = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;
const DEBOUNCE_MS = 400;
export const maxSlugNameCharacterCount = 64;
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
      switchMap((name) =>
        name
          ? timer(DEBOUNCE_MS).pipe(
              switchMap(() => isAvailable(name)),
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
      const name = slugify(displayName).slice(0, maxSlugNameCharacterCount).replace(/-+$/, '');
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
