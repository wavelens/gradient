/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, effect, inject, input, computed, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';
import { AbstractControl, NgControl } from '@angular/forms';

type FieldControl = AbstractControl | NgControl | null;

/// Works with template-driven forms as well as reactive ones, because most of
/// the app binds ngModel against plain objects and has no AbstractControl to pass.
@Component({
  selector: 'gr-form-field',
  standalone: true,
  templateUrl: './form-field.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './form-field.component.scss',
})
export class FormFieldComponent {
  label = input<string>();
  for = input<string>();
  hint = input<string>();
  required = input(false, { transform: booleanAttribute });
  control = input<FieldControl>(null);
  invalid = input<boolean | null>(null);
  error = input<string>();
  errorMessages = input<Record<string, string>>({});

  showError = computed(() => {
    const explicit = this.invalid();
    if (explicit !== null) return explicit;
    const ctrl = this.control();
    return !!ctrl?.invalid && !!ctrl?.touched;
  });

  messageId = computed(() => `${this.for() ?? 'gr-field'}-message`);

  /// The projected control is owned by the caller, so the error semantics are
  /// applied to it here rather than duplicated at every call site.
  private hostRef = inject(ElementRef<HTMLElement>);

  constructor() {
    effect(() => {
      const control = (this.hostRef.nativeElement as HTMLElement).querySelector<HTMLElement>(
        'input, textarea, select, [role=combobox]',
      );
      if (!control) return;
      const invalid = this.showError();
      control.setAttribute('aria-invalid', String(invalid));
      control.setAttribute('aria-describedby', this.messageId());
      if (this.required()) control.setAttribute('aria-required', 'true');
    });
  }

  errorText = computed(() => {
    const explicit = this.error();
    if (explicit) return explicit;
    const errors = this.control()?.errors;
    if (!errors) return '';
    return this.errorMessages()[Object.keys(errors)[0]] ?? '';
  });
}
