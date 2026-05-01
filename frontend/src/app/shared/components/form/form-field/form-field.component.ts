/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input } from '@angular/core';
import { CommonModule } from '@angular/common';
import { AbstractControl } from '@angular/forms';
import { FormErrorComponent } from '../form-error/form-error.component';

@Component({
  selector: 'gr-form-field',
  standalone: true,
  imports: [CommonModule, FormErrorComponent],
  templateUrl: './form-field.component.html',
  styleUrl: './form-field.component.scss',
})
export class FormFieldComponent {
  label = input<string>();
  for = input<string>();
  hint = input<string>();
  required = input<boolean>(false);
  control = input<AbstractControl | null>(null);
  errorMessages = input<Record<string, string>>({});
}
