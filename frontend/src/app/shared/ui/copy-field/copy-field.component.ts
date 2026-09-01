/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, signal, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ButtonComponent } from '../button/button.component';
import { InputDirective } from '../input/input.directive';

@Component({
  selector: 'gr-copy-field',
  standalone: true,
  imports: [CommonModule, ButtonComponent, InputDirective],
  templateUrl: './copy-field.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './copy-field.component.scss',
})
export class CopyFieldComponent {
  value = input.required<string>();
  id = input<string>();
  mono = input(true, { transform: booleanAttribute });
  inline = input(false, { transform: booleanAttribute });
  multiline = input(false, { transform: booleanAttribute });
  rows = input<number>(4);

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
