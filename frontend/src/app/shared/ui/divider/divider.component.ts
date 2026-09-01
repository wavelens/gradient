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
  styleUrl: './divider.component.scss',
})
export class DividerComponent {
  orientation = input<'horizontal' | 'vertical'>('horizontal');
}
