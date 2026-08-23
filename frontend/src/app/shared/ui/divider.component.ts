/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';

@Component({
  selector: 'gr-divider',
  standalone: true,
  template: '',
  host: {
    role: 'separator',
    '[attr.aria-orientation]': 'orientation()',
    '[class.gr-divider--vertical]': 'orientation() === "vertical"',
  },
  changeDetection: ChangeDetectionStrategy.Eager,
  styles: [
    `
      :host {
        display: block;
        margin: 1rem 0;
        border-top: 1px solid #2d333b;
      }
      :host(.gr-divider--vertical) {
        display: inline-block;
        align-self: stretch;
        margin: 0 1rem;
        border-top: 0;
        border-left: 1px solid #2d333b;
      }
    `,
  ],
})
export class DividerComponent {
  orientation = input<'horizontal' | 'vertical'>('horizontal');
}
