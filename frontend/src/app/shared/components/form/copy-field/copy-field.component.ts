/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';

@Component({
  selector: 'gr-copy-field',
  standalone: true,
  imports: [CommonModule, ButtonModule, InputTextModule],
  templateUrl: './copy-field.component.html',
  styleUrl: './copy-field.component.scss',
})
export class CopyFieldComponent {
  value = input.required<string>();
  id = input<string>();
  mono = input<boolean>(true);

  copied = signal(false);

  async copy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(this.value());
      this.copied.set(true);
      setTimeout(() => this.copied.set(false), 1500);
    } catch {
      // Clipboard API may be denied; silently fail. The user can still select & copy.
    }
  }
}
