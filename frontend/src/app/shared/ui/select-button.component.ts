/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';
import { Component, booleanAttribute, forwardRef, input, signal, ChangeDetectionStrategy } from '@angular/core';

@Component({
  selector: 'gr-select-button',
  standalone: true,
  template: `
    <div class="gr-select-button" role="radiogroup">
      @for (option of options(); track $index) {
        <button
          type="button"
          role="radio"
          class="gr-select-button__item"
          [class.gr-select-button__item--selected]="isSelected(option)"
          [attr.aria-checked]="isSelected(option)"
          [disabled]="isDisabled()"
          (click)="pick(option)"
        >
          {{ labelOf(option) }}
        </button>
      }
    </div>
  `,
  styleUrl: './select-button.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => SelectButtonComponent), multi: true },
  ],
})
export class SelectButtonComponent implements ControlValueAccessor {
  options = input<readonly unknown[]>([]);
  optionLabel = input('label');
  optionValue = input('value');
  allowEmpty = input(true, { transform: booleanAttribute });

  protected value = signal<unknown>(null);
  protected isDisabled = signal(false);
  private onChange: (value: unknown) => void = () => {};
  private onTouched: () => void = () => {};

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
    const key = this.optionLabel();
    if (option && typeof option === 'object' && key) return String((option as never)[key] ?? '');
    return String(option ?? '');
  }

  protected valueOf(option: unknown): unknown {
    const key = this.optionValue();
    if (option && typeof option === 'object' && key) return (option as never)[key];
    return option;
  }

  protected isSelected(option: unknown): boolean {
    return this.valueOf(option) === this.value();
  }

  protected pick(option: unknown): void {
    const next = this.valueOf(option);
    if (next === this.value() && !this.allowEmpty()) return;
    this.value.set(next === this.value() && this.allowEmpty() ? null : next);
    this.onChange(this.value());
    this.onTouched();
  }
}
