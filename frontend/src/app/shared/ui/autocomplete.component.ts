/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { CdkConnectedOverlay, CdkOverlayOrigin } from '@angular/cdk/overlay';
import { ControlValueAccessor, NG_VALUE_ACCESSOR } from '@angular/forms';
import {
  Component,
  ElementRef,
  booleanAttribute,
  computed,
  forwardRef,
  inject,
  input,
  output,
  signal,
} from '@angular/core';

@Component({
  selector: 'gr-autocomplete',
  standalone: true,
  imports: [CdkConnectedOverlay, CdkOverlayOrigin],
  templateUrl: './autocomplete.component.html',
  styleUrl: './autocomplete.component.scss',
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => AutoCompleteComponent), multi: true },
  ],
})
export class AutoCompleteComponent implements ControlValueAccessor {
  suggestions = input<readonly string[]>([]);
  placeholder = input('');
  inputId = input('');
  forceSelection = input(false, { transform: booleanAttribute });

  completeMethod = output<{ query: string }>();
  enter = output<void>();

  protected open = signal(false);
  protected value = signal('');
  protected isDisabled = signal(false);

  private host = inject(ElementRef<HTMLElement>);
  private onChange: (value: string) => void = () => {};
  protected onTouched: () => void = () => {};

  protected panelWidth = computed(() => this.open() && this.host.nativeElement.offsetWidth);

  writeValue(value: string): void {
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
    const query = (event.target as HTMLInputElement).value;
    this.value.set(query);
    if (!this.forceSelection()) this.onChange(query);
    this.completeMethod.emit({ query });
    this.open.set(true);
  }

  protected pick(suggestion: string): void {
    this.value.set(suggestion);
    this.onChange(suggestion);
    this.open.set(false);
  }

  protected close(): void {
    this.open.set(false);
    this.onTouched();
  }

  protected onEnter(): void {
    this.open.set(false);
    this.enter.emit();
  }
}
