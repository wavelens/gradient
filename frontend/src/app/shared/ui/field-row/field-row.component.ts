/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';

/// Read-only counterpart to gr-form-field: a label with a value the user cannot edit.
@Component({
  selector: 'gr-field-row',
  standalone: true,
  templateUrl: './field-row.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './field-row.component.scss',
})
export class FieldRowComponent {
  label = input.required<string>();
  value = input<string>();
  mono = input(false, { transform: booleanAttribute });
}
