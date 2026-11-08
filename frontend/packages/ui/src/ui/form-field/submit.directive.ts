/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, Injector, afterNextRender, computed, inject, output } from '@angular/core';
import { FormErrors } from './form-errors';

/// Binding: `(grSubmit)`, not `(click)`. The directive can fire only in the same settings section or dialog with zero gr-form-field components that carry an error. Move focus to the first field with an error after each click. A rejected submit can then guide the user to the problem.
@Directive({
  selector: '[grSubmit]',
  standalone: true,
  host: { '(click)': 'submitUnlessBlocked()' },
})
export class SubmitDirective {
  grSubmit = output<void>();

  private formErrors = inject(FormErrors, { optional: true });
  private injector = inject(Injector);

  private readonly blocked = computed(() => this.formErrors?.shown() ?? false);

  protected submitUnlessBlocked(): void {
    if (!this.blocked()) this.grSubmit.emit();
    afterNextRender(() => this.formErrors?.revealFirst(), { injector: this.injector });
  }
}
