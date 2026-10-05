/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';

// Stands in for content that has not arrived; the loading container carries aria-busy, so this stays hidden.
@Component({
  selector: 'gr-skeleton',
  standalone: true,
  template: '',
  host: {
    'aria-hidden': 'true',
    '[style.width]': 'width()',
    '[style.height]': 'height()',
  },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './skeleton.component.scss',
})
export class SkeletonComponent {
  width = input('100%');
  height = input('1em');
}
