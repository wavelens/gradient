/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, forwardRef, input, signal, ChangeDetectionStrategy } from '@angular/core';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';

/// Switches what a panel shows: separate tabs, one of them current. Distinct
/// from gr-select-button, which is a joined control for a form field.
@Component({
  selector: 'gr-tab-switch',
  standalone: true,
  templateUrl: './tab-switch.component.html',
  styleUrl: './tab-switch.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => TabSwitchComponent), multi: true },
  ],
})
export class TabSwitchComponent implements ControlValueAccessor {
  options = input<readonly unknown[]>([]);
  optionLabel = input('label');
  optionValue = input('value');
  ariaLabel = input<string>();

  protected value = signal<unknown>(null);
  protected isDisabled = signal(false);

  private onChange: (value: unknown) => void = () => {};
  protected onTouched: () => void = () => {};

  writeValue(value: unknown): void {
    this.value.set(value ?? null);
  }

  registerOnChange(fn: (value: unknown) => void): void {
    this.onChange = fn;
  }

  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }

  setDisabledState(disabled: boolean): void {
    this.isDisabled.set(disabled);
  }

  protected labelOf(option: unknown): string {
    return String((option as Record<string, unknown>)?.[this.optionLabel()] ?? option);
  }

  protected valueOf(option: unknown): unknown {
    const key = this.optionValue();
    return key && option && typeof option === 'object'
      ? (option as Record<string, unknown>)[key]
      : option;
  }

  protected isSelected(option: unknown): boolean {
    return this.valueOf(option) === this.value();
  }

  protected pick(option: unknown): void {
    const next = this.valueOf(option);
    this.value.set(next);
    this.onChange(next);
    this.onTouched();
  }
}
