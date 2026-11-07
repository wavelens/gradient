/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, inject } from '@angular/core';
import { Observable, defaultIfEmpty, of } from 'rxjs';
import { ConfirmService } from '@shared/confirm';

export interface UnsavedChanges {
  mark(): void;
  changed(): boolean;
  canLeave(): boolean | Observable<boolean>;
}

/// Use this helper to track the settings form against the last saved state. Call `mark()` after load and after each save. Create the helper in an injection context. The helper can warn before the browser tab closes and before in-app navigation through `canLeave()`.
export function unsavedChanges(read: () => unknown, page: string): UnsavedChanges {
  const confirm = inject(ConfirmService);
  let saved: string | null = null;
  const snapshot = () => JSON.stringify(read());
  const changed = () => saved !== null && snapshot() !== saved;

  const beforeUnload = (event: BeforeUnloadEvent) => {
    if (changed()) event.preventDefault();
  };
  window.addEventListener('beforeunload', beforeUnload);
  inject(DestroyRef).onDestroy(() => window.removeEventListener('beforeunload', beforeUnload));

  return {
    mark: () => (saved = snapshot()),
    changed,
    canLeave: () =>
      !changed() ||
      confirm
        .confirm({
          title: 'Leave Without Saving',
          target: page,
          consequence: 'All edits disappear.',
          confirmLabel: 'Leave',
          severity: 'warn',
          failure: 'All edits disappear.',
          action: () => of(true),
        })
        .pipe(defaultIfEmpty(false)),
  };
}
