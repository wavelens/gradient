/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy, input } from '@angular/core';

/// The lambda mark, masked so it takes the logo colour of the theme. The dark
/// theme adds the white border of the outlined drawing behind it.
@Component({
  selector: 'gr-logo',
  standalone: true,
  template: `
    @if (src(); as url) {
      <img class="custom" [src]="url" alt="Logo" />
    } @else {
      <span class="mark" role="img" aria-label="Gradient"></span>
    }
  `,
  styleUrl: './logo.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class LogoComponent {
  src = input<string | null>(null);
}
