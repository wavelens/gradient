/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { injectOnScreen } from '../on-screen/on-screen';

// Stands in for content that has not arrived; the loading container carries aria-busy, so this stays hidden.
@Component({
  selector: 'gr-skeleton',
  standalone: true,
  template: '',
  host: {
    'aria-hidden': 'true',
    '[class.still]': '!onScreen()',
    '[style.width]': 'width()',
    '[style.height]': 'height()',
  },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './skeleton.component.scss',
})
export class SkeletonComponent {
  width = input<string>();
  height = input<string>();
  protected readonly onScreen = injectOnScreen();
}
