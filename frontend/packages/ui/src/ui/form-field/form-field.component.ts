/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, DestroyRef, ElementRef, effect, inject, input, computed, signal, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';
import { AbstractControl, NgControl } from '@angular/forms';
import { FormErrors } from './form-errors';
import { NgTemplateOutlet } from '@angular/common';
import { LabelExplanationButtonComponent } from '../label-explanation-button/label-explanation-button.component';

type FieldControl = AbstractControl | NgControl | null;

let formFieldMessageSeq = 0;

/// Works with template-driven forms as well as reactive ones, because most of
/// the app binds ngModel against plain objects and has no AbstractControl to pass.
@Component({
  selector: 'gr-form-field',
  imports: [NgTemplateOutlet, LabelExplanationButtonComponent],
  standalone: true,
  templateUrl: './form-field.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './form-field.component.scss',
})
export class FormFieldComponent {
  label = input<string>();
  for = input<string>();
  hint = input<string>();
  warning = input<string | null>(null);
  required = input(false, { transform: booleanAttribute });
  control = input<FieldControl>(null);
  invalid = input<boolean | null>(null);
  error = input<string>();
  errorMessages = input<Record<string, string>>({});

  private controlChanges = signal(0);

  showError = computed(() => {
    const explicit = this.invalid();
    if (explicit !== null) return explicit;
    this.controlChanges();
    const ctrl = this.control();
    return !!ctrl?.invalid && !!ctrl?.touched;
  });

  private readonly messageElementIdWhenForInputIsMissing = `gr-field-${formFieldMessageSeq++}-message`;
  messageId = computed(() => {
    const id = this.for();
    return id ? `${id}-message` : this.messageElementIdWhenForInputIsMissing;
  });
  hintTextId = computed(() => `${this.messageId()}-hint`);
  helpButtonAccessibleName = computed(() => {
    const label = this.label();
    return label ? `About ${label}` : 'About this setting';
  });

  /// The projected control is owned by the caller, so the error semantics are
  /// applied to it here rather than duplicated at every call site.
  private hostRef = inject(ElementRef<HTMLElement>);

  constructor() {
    inject(FormErrors, { optional: true })?.track(
      { errorShown: this.showError, element: this.hostRef.nativeElement },
      inject(DestroyRef),
    );
    effect((onCleanup) => {
      const ctrl = this.control();
      const events = (ctrl instanceof NgControl ? ctrl.control : ctrl)?.events;
      if (!events) return;
      const subscription = events.subscribe(() => this.controlChanges.update((n) => n + 1));
      onCleanup(() => subscription.unsubscribe());
    });
    effect(() => {
      const control = (this.hostRef.nativeElement as HTMLElement).querySelector<HTMLElement>(
        'input, textarea, select, [role=combobox]',
      );
      if (!control) return;
      const invalid = this.showError();
      control.setAttribute('aria-invalid', String(invalid));
      control.setAttribute('aria-describedby', this.hint() ? `${this.hintTextId()} ${this.messageId()}` : this.messageId());
      if (this.required()) control.setAttribute('aria-required', 'true');
    });
  }

  errorText = computed(() => {
    const explicit = this.error();
    if (explicit) return explicit;
    this.controlChanges();
    const errors = this.control()?.errors;
    if (!errors) return '';
    return this.errorMessages()[Object.keys(errors)[0]] ?? '';
  });
}
