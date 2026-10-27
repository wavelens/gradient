/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, Injectable, Signal, computed, signal } from '@angular/core';

@Injectable()
export class FormErrors {
  private fields = signal<readonly Signal<boolean>[]>([]);

  readonly shown = computed(() => this.fields().some((field) => field()));

  track(errorShown: Signal<boolean>, destroyRef: DestroyRef): void {
    this.fields.update((fields) => [...fields, errorShown]);
    destroyRef.onDestroy(() => this.fields.update((fields) => fields.filter((f) => f !== errorShown)));
  }
}
