/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';

@Component({
  selector: 'gr-card-grid',
  standalone: true,
  template: '<ng-content></ng-content>',
  changeDetection: ChangeDetectionStrategy.Eager,
  host: { '[style.--gr-card-min]': 'min()' },
  styleUrl: './card-grid.component.scss',
})
export class CardGridComponent {
  min = input<string>('400px');
}
