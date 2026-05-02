/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input } from '@angular/core';
import { CommonModule } from '@angular/common';

@Component({
  selector: 'gr-label-help',
  standalone: true,
  imports: [CommonModule],
  template: `
    <a
      class="label-help"
      [href]="href()"
      target="_blank"
      rel="noopener noreferrer"
      [title]="title()"
      [attr.aria-label]="title()"
    >
      <span class="material-symbols-outlined">help</span>
    </a>
  `,
  styleUrl: './label-help.component.scss',
})
export class LabelHelpComponent {
  href = input.required<string>();
  title = input<string>('Learn more');
}
