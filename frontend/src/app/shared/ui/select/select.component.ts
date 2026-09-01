/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { CdkConnectedOverlay, CdkOverlayOrigin } from '@angular/cdk/overlay';
import { IconComponent } from '../icon/icon.component';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';
import {
  Component,
  ElementRef,
  computed,
  forwardRef,
  inject,
  input,
  signal,
  ChangeDetectionStrategy
} from '@angular/core';

/// Single-select dropdown on the CDK connected overlay. `optionLabel` and
/// `optionValue` read plain `{ label, value }` option arrays straight from callers.
@Component({
  selector: 'gr-select',
  standalone: true,
  imports: [IconComponent, CdkConnectedOverlay, CdkOverlayOrigin],
  templateUrl: './select.component.html',
  styleUrl: './select.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => SelectComponent), multi: true },
  ],
})
export class SelectComponent implements ControlValueAccessor {
  options = input<readonly unknown[]>([]);
  optionLabel = input('label');
  optionValue = input('value');
  optionDisabled = input('');
  placeholder = input('');
  inputId = input('');

  protected open = signal(false);
  protected value = signal<unknown>(null);
  protected isDisabled = signal(false);
  protected activeIndex = signal(-1);

  private host = inject(ElementRef<HTMLElement>);
  private onChange: (value: unknown) => void = () => {};
  protected onTouched: () => void = () => {};

  protected triggerWidth = computed(() => this.open() && this.host.nativeElement.offsetWidth);

  protected selectedLabel = computed(() => {
    const match = this.options().find((o) => this.valueOf(o) === this.value());
    return match === undefined ? '' : this.labelOf(match);
  });

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
    if (disabled) this.open.set(false);
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

  protected isDisabledOption(option: unknown): boolean {
    const key = this.optionDisabled();
    return !!key && !!option && typeof option === 'object' && !!(option as never)[key];
  }

  protected isSelected(option: unknown): boolean {
    return this.valueOf(option) === this.value();
  }

  protected toggle(): void {
    if (this.isDisabled()) return;
    this.open.update((o) => !o);
    if (this.open()) this.activeIndex.set(Math.max(0, this.options().findIndex((o) => this.isSelected(o))));
  }

  protected close(): void {
    this.open.set(false);
    this.onTouched();
  }

  protected pick(option: unknown): void {
    if (this.isDisabledOption(option)) return;
    this.value.set(this.valueOf(option));
    this.onChange(this.value());
    this.close();
  }

  protected onKeydown(event: KeyboardEvent): void {
    if (this.isDisabled()) return;
    const items = this.options();
    if (event.key === 'Escape') return this.close();
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (!this.open()) return this.toggle();
      const step = event.key === 'ArrowDown' ? 1 : -1;
      const next = Math.min(items.length - 1, Math.max(0, this.activeIndex() + step));
      this.activeIndex.set(next);
      return;
    }
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (!this.open()) return this.toggle();
      const active = items[this.activeIndex()];
      if (active !== undefined) this.pick(active);
    }
  }
}
