/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, signal, ChangeDetectionStrategy } from '@angular/core';
import { IconComponent } from '../icon/icon.component';
import { CommonModule } from '@angular/common';
import { FormControl, ReactiveFormsModule } from '@angular/forms';

@Component({
  selector: 'gr-password-input',
  standalone: true,
  imports: [IconComponent, CommonModule, ReactiveFormsModule],
  templateUrl: './password-input.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
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
