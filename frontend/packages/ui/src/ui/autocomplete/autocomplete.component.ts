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
  OnDestroy,
  booleanAttribute,
  computed,
  forwardRef,
  inject,
  input,
  output,
  signal,
  ChangeDetectionStrategy
} from '@angular/core';
import { FormFieldComponent } from '../form-field/form-field.component';

export interface AutoCompleteSuggestion {
  label: string;
  value: string;
  textBelowLabel?: string;
}

let autocompleteListboxIdCounter = 0;

function toSuggestion(item: string | AutoCompleteSuggestion): AutoCompleteSuggestion {
  return typeof item === 'string' ? { label: item, value: item } : item;
}

@Component({
  selector: 'gr-autocomplete',
  standalone: true,
  imports: [CdkConnectedOverlay, CdkOverlayOrigin],
  templateUrl: './autocomplete.component.html',
  styleUrl: './autocomplete.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
  providers: [
    { provide: NG_VALUE_ACCESSOR, useExisting: forwardRef(() => AutoCompleteComponent), multi: true },
  ],
})
export class AutoCompleteComponent implements ControlValueAccessor, OnDestroy {
  suggestions = input<readonly (string | AutoCompleteSuggestion)[]>([]);
  placeholder = input('');
  inputId = input('');
  forceSelection = input(false, { transform: booleanAttribute });

  completeMethod = output<{ query: string }>();
  enter = output<void>();

  protected open = signal(false);
  protected text = signal('');
  protected isDisabled = signal(false);
  protected activeIndex = signal(-1);
  protected autocompleteListboxId = `gr-autocomplete-listbox-${autocompleteListboxIdCounter++}`;

  private host = inject(ElementRef<HTMLElement>);
  private field = inject(FormFieldComponent, { optional: true });
  private onChange: (value: string) => void = () => {};
  protected onTouched: () => void = () => {};

  protected options = computed(() => this.suggestions().map(toSuggestion));
  protected expanded = computed(() => this.open() && this.options().length > 0);
  protected suggestionListboxAccessibleName = computed(() => this.field?.label() || this.placeholder() || null);
  protected panelWidth = computed(() => this.open() && this.host.nativeElement.offsetWidth);

  writeValue(value: string): void {
    const modelValue = value ?? '';
    this.text.set(this.options().find((o) => o.value === modelValue)?.label ?? modelValue);
  }

  registerOnChange(fn: (value: string) => void): void {
    this.onChange = fn;
  }

  registerOnTouched(fn: () => void): void {
    this.onTouched = fn;
  }

  setDisabledState(disabled: boolean): void {
    this.isDisabled.set(disabled);
    if (disabled) this.open.set(false);
  }

  ngOnDestroy(): void {
    this.open.set(false);
  }

  protected optionId(index: number): string {
    return `${this.autocompleteListboxId}-${index}`;
  }

  protected onInput(event: Event): void {
    const query = (event.target as HTMLInputElement).value;
    this.text.set(query);
    if (!this.forceSelection()) this.onChange(query);
    this.activeIndex.set(-1);
    this.completeMethod.emit({ query });
    this.open.set(true);
  }

  protected openSuggestionPanelAndRequestSuggestionsForCurrentText(): void {
    if (this.isDisabled() || this.open()) return;
    this.activeIndex.set(-1);
    this.completeMethod.emit({ query: this.text() });
    this.open.set(true);
  }

  protected onKeydown(event: KeyboardEvent): void {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (!this.open()) return this.openSuggestionPanelAndRequestSuggestionsForCurrentText();
      const last = this.options().length - 1;
      const step = event.key === 'ArrowDown' ? 1 : -1;
      this.activeIndex.update((index) => Math.min(last, Math.max(0, index + step)));
      return;
    }
    if (event.key === 'Escape' && this.expanded()) {
      event.stopPropagation();
      this.open.set(false);
      return;
    }
    if (event.key === 'Enter') {
      const active = this.expanded() ? this.options()[this.activeIndex()] : undefined;
      if (active) {
        event.preventDefault();
        this.pick(active);
        return;
      }
      this.open.set(false);
      this.enter.emit();
    }
  }

  protected pick(suggestion: AutoCompleteSuggestion): void {
    this.text.set(suggestion.label);
    this.onChange(suggestion.value);
    this.open.set(false);
  }

  protected onBlur(): void {
    this.open.set(false);
    this.onTouched();
  }
}
