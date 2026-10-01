/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, booleanAttribute, computed, forwardRef, input, signal, ChangeDetectionStrategy } from '@angular/core';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';
import { FormFieldComponent } from '../form-field/form-field.component';
import { IconComponent } from '../icon/icon.component';
import { InputDirective } from '../input/input.directive';

export type NameCheckState = 'idle' | 'checking' | 'available' | 'taken' | 'invalid' | 'reserved';

const MESSAGES: Partial<Record<NameCheckState, string>> = {
  taken: 'This name is already taken.',
  reserved: 'This name is reserved.',
  invalid: 'Lowercase letters, numbers, and hyphens only. Cannot start or end with a hyphen.',
};

/// A slug field whose availability is checked as you type. The caller owns the
/// check and reports its state; the wording of every outcome lives here.
@Component({
  selector: 'gr-name-field',
  standalone: true,
  imports: [FormFieldComponent, IconComponent, InputDirective],
  templateUrl: './name-field.component.html',
  styleUrl: './name-field.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => NameFieldComponent), multi: true },
  ],
})
export class NameFieldComponent implements ControlValueAccessor {
  label = input('Name');
  inputId = input('');
  placeholder = input('');
  hint = input('Lowercase letters, numbers, and hyphens only');
  state = input<NameCheckState>('idle');
  required = input(false, { transform: booleanAttribute });

  protected value = signal('');
  protected isDisabled = signal(false);

  protected error = computed(() => MESSAGES[this.state()] ?? '');
  protected invalid = computed(() => !!MESSAGES[this.state()]);

  private onChange: (value: string) => void = () => {};
  protected onTouched: () => void = () => {};

  writeValue(value: string | null): void {
    this.value.set(value ?? '');
  }

  registerOnChange(fn: (value: string) => void): void {
    this.onChange = fn;
  }

  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }

  setDisabledState(disabled: boolean): void {
    this.isDisabled.set(disabled);
  }

  protected onInput(event: Event): void {
    const next = (event.target as HTMLInputElement).value;
    this.value.set(next);
    this.onChange(next);
  }
}
