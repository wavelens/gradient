/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import {
  AbstractControl,
  FormBuilder,
  FormControl,
  ValidationErrors,
  ValidatorFn,
  Validators,
} from '@angular/forms';

export interface TextFieldOptions {
  required?: boolean;
  minLength?: number;
  maxLength?: number;
  pattern?: string | RegExp;
  validators?: ValidatorFn[];
}

export interface PasswordFieldOptions extends TextFieldOptions {
  strength?: boolean;
}

export const passwordStrengthValidator: ValidatorFn = (
  control: AbstractControl,
): ValidationErrors | null => {
  const value = String(control.value ?? '');
  if (!value) return null;
  const hasUpper = /[A-Z]/.test(value);
  const hasLower = /[a-z]/.test(value);
  const hasDigit = /\d/.test(value);
  const hasMinLen = value.length >= 8;
  return hasUpper && hasLower && hasDigit && hasMinLen ? null : { passwordStrength: true };
};

export function passwordMatchValidator(otherControlName: string): ValidatorFn {
  return (control: AbstractControl): ValidationErrors | null => {
    const parent = control.parent;
    if (!parent) return null;
    const other = parent.get(otherControlName);
    if (!other) return null;
    return other.value === control.value ? null : { passwordMatch: true };
  };
}

@Injectable({ providedIn: 'root' })
export class FormFieldsBuilder {
  private fb = inject(FormBuilder);

  text(initial = '', opts: TextFieldOptions = {}): FormControl<string> {
    return this.fb.nonNullable.control(initial, this.buildValidators(opts));
  }

  email(initial = '', opts: TextFieldOptions = { required: true }): FormControl<string> {
    return this.fb.nonNullable.control(initial, [
      ...this.buildValidators(opts),
      Validators.email,
    ]);
  }

  password(initial = '', opts: PasswordFieldOptions = { required: true }): FormControl<string> {
    const validators = this.buildValidators(opts);
    if (opts.strength) validators.push(passwordStrengthValidator);
    return this.fb.nonNullable.control(initial, validators);
  }

  confirm(otherControlName: string, initial = ''): FormControl<string> {
    return this.fb.nonNullable.control(initial, [
      Validators.required,
      passwordMatchValidator(otherControlName),
    ]);
  }

  number(initial: number | null = null, opts: { required?: boolean; min?: number; max?: number; validators?: ValidatorFn[] } = {}): FormControl<number | null> {
    const validators: ValidatorFn[] = [];
    if (opts.required) validators.push(Validators.required);
    if (opts.min !== undefined) validators.push(Validators.min(opts.min));
    if (opts.max !== undefined) validators.push(Validators.max(opts.max));
    if (opts.validators) validators.push(...opts.validators);
    return this.fb.control(initial, validators);
  }

  checkbox(initial = false): FormControl<boolean> {
    return this.fb.nonNullable.control(initial);
  }

  private buildValidators(opts: TextFieldOptions): ValidatorFn[] {
    const v: ValidatorFn[] = [];
    if (opts.required) v.push(Validators.required);
    if (opts.minLength !== undefined) v.push(Validators.minLength(opts.minLength));
    if (opts.maxLength !== undefined) v.push(Validators.maxLength(opts.maxLength));
    if (opts.pattern !== undefined) v.push(Validators.pattern(opts.pattern));
    if (opts.validators) v.push(...opts.validators);
    return v;
  }
}
