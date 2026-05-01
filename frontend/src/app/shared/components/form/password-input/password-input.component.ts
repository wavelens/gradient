/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormControl, ReactiveFormsModule } from '@angular/forms';

@Component({
  selector: 'gr-password-input',
  standalone: true,
  imports: [CommonModule, ReactiveFormsModule],
  templateUrl: './password-input.component.html',
  styleUrl: './password-input.component.scss',
})
export class PasswordInputComponent {
  control = input.required<FormControl>();
  id = input<string>();
  placeholder = input<string>('');
  autocomplete = input<string>('current-password');

  show = signal(false);

  toggle(): void {
    this.show.update((v) => !v);
  }
}
