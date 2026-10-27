/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, computed, inject, output } from '@angular/core';
import { FormErrors } from './form-errors';

/// Bound as `(grSubmit)` instead of `(click)`: it fires only while no
/// gr-form-field in the same settings section or dialog shows an error.
@Directive({
  selector: '[grSubmit]',
  standalone: true,
  host: { '(click)': 'submitUnlessBlocked()' },
})
export class SubmitDirective {
  grSubmit = output<void>();

  private formErrors = inject(FormErrors, { optional: true });

  readonly blocked = computed(() => this.formErrors?.shown() ?? false);

  protected submitUnlessBlocked(): void {
    if (!this.blocked()) this.grSubmit.emit();
  }
}
