/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input } from '@angular/core';
import { CommonModule } from '@angular/common';
import { AbstractControl } from '@angular/forms';

const DEFAULT_MESSAGES: Record<string, (err: unknown) => string> = {
  required: () => 'This field is required.',
  email: () => 'Enter a valid email address.',
  minlength: (e: unknown) => {
    const v = e as { requiredLength?: number };
    return `Must be at least ${v.requiredLength ?? 0} characters.`;
  },
  maxlength: (e: unknown) => {
    const v = e as { requiredLength?: number };
    return `Must be at most ${v.requiredLength ?? 0} characters.`;
  },
  min: (e: unknown) => {
    const v = e as { min?: number };
    return `Must be at least ${v.min ?? 0}.`;
  },
  max: (e: unknown) => {
    const v = e as { max?: number };
    return `Must be at most ${v.max ?? 0}.`;
  },
  pattern: () => 'Value does not match the required format.',
  passwordStrength: () => 'Password does not meet strength requirements.',
  passwordMatch: () => 'Passwords do not match.',
  usernameTaken: () => 'This username is already taken.',
};

@Component({
  selector: 'gr-form-error',
  standalone: true,
  imports: [CommonModule],
  template: `
    @if (visibleMessage(); as msg) {
      <span class="form-error" role="alert">{{ msg }}</span>
    }
  `,
  styleUrl: './form-error.component.scss',
})
export class FormErrorComponent {
  control = input<AbstractControl | null>(null);
  messages = input<Record<string, string>>({});

  visibleMessage = computed<string | null>(() => {
    const c = this.control();
    if (!c || !c.errors || (!c.touched && !c.dirty)) return null;
    const overrides = this.messages();
    for (const [key, err] of Object.entries(c.errors)) {
      if (overrides[key]) return overrides[key];
      const fmt = DEFAULT_MESSAGES[key];
      if (fmt) return fmt(err);
    }
    return 'Invalid value.';
  });
}
