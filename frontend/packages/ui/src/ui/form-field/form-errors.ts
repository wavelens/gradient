/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, Injectable, Signal, computed, signal } from '@angular/core';

interface TrackedField {
  errorShown: Signal<boolean>;
  element: HTMLElement;
}

export function revealFirstInvalid(root: ParentNode): boolean {
  return focusInvalid(root.querySelector<HTMLElement>('[aria-invalid="true"]'));
}

function focusInvalid(control: HTMLElement | null | undefined): boolean {
  if (!control) return false;
  control.scrollIntoView({ block: 'nearest' });
  control.focus({ preventScroll: true });
  return true;
}

@Injectable()
export class FormErrors {
  private fields = signal<readonly TrackedField[]>([]);

  readonly shown = computed(() => this.fields().some((field) => field.errorShown()));

  track(field: TrackedField, destroyRef: DestroyRef): void {
    this.fields.update((fields) => [...fields, field]);
    destroyRef.onDestroy(() => this.fields.update((fields) => fields.filter((f) => f !== field)));
  }

  revealFirst(): boolean {
    const [first] = this.fields()
      .filter((field) => field.errorShown())
      .map((field) => field.element)
      .sort((a, b) => (a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1));
    return focusInvalid(first?.querySelector<HTMLElement>('[aria-invalid="true"]'));
  }
}
