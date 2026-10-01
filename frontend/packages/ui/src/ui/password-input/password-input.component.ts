/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, forwardRef, input, signal, ChangeDetectionStrategy } from '@angular/core';
import { IconComponent } from '../icon/icon.component';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';

/// Password field with a reveal toggle. A value accessor like every other
/// control, so it binds through `ngModel` and reactive forms alike.
@Component({
  selector: 'gr-password-input',
  standalone: true,
  imports: [IconComponent],
  templateUrl: './password-input.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './password-input.component.scss',
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => PasswordInputComponent), multi: true },
  ],
})
export class PasswordInputComponent implements ControlValueAccessor {
  inputId = input('');
  placeholder = input('');
  autocomplete = input('current-password');

  protected value = signal('');
  protected isDisabled = signal(false);
  protected show = signal(false);

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

  protected toggle(): void {
    this.show.update((v) => !v);
  }
}
