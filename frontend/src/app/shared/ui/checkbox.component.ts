/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { NG_VALUE_ACCESSOR, ControlValueAccessor } from '@angular/forms';
import { Component, booleanAttribute, forwardRef, input, signal, ChangeDetectionStrategy } from '@angular/core';

@Component({
  selector: 'gr-checkbox',
  standalone: true,
  template: `
    <input
      type="checkbox"
      class="gr-checkbox__input"
      [id]="inputId()"
      [checked]="checked()"
      [disabled]="isDisabled()"
      (change)="onToggle($event)"
      (blur)="onTouched()"
    />
  `,
  styleUrl: './checkbox.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => CheckboxComponent), multi: true },
  ],
})
export class CheckboxComponent implements ControlValueAccessor {
  inputId = input('');
  binary = input(true, { transform: booleanAttribute });

  protected checked = signal(false);
  protected isDisabled = signal(false);
  private onChange: (value: boolean) => void = () => {};
  protected onTouched: () => void = () => {};

  writeValue(value: boolean): void {
    this.checked.set(!!value);
  }

  registerOnChange(fn: (value: boolean) => void): void {
    this.onChange = fn;
  }

  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }

  setDisabledState(disabled: boolean): void {
    this.isDisabled.set(disabled);
  }

  protected onToggle(event: Event): void {
    const next = (event.target as HTMLInputElement).checked;
    this.checked.set(next);
    this.onChange(next);
  }
}
