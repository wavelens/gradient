/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, computed, ChangeDetectionStrategy } from '@angular/core';
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
  required = input<boolean>(false);
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

  errorText = computed(() => {
    const explicit = this.error();
    if (explicit) return explicit;
    const errors = this.control()?.errors;
    if (!errors) return '';
    return this.errorMessages()[Object.keys(errors)[0]] ?? '';
  });
}
