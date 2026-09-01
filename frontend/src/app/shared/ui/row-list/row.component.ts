/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';
import { IconComponent } from '../icon/icon.component';

@Component({
  selector: 'gr-row',
  standalone: true,
  imports: [IconComponent],
  templateUrl: './row.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './row.component.scss',
})
export class RowComponent {
  icon = input<string>();
  muted = input<boolean>(false);
}
