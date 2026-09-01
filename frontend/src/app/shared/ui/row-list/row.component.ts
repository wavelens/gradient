/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';
import { RouterLink } from '@angular/router';
import { IconComponent } from '../icon/icon.component';

/// One entity per row. Given a `link`, the row itself navigates and grows a
/// chevron, so a settings destination needs no button of its own.
@Component({
  selector: 'gr-row',
  standalone: true,
  imports: [IconComponent, RouterLink],
  templateUrl: './row.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './row.component.scss',
})
export class RowComponent {
  icon = input<string>();
  muted = input(false, { transform: booleanAttribute });
  link = input<unknown[]>();
  /// Names the destination for assistive tech, since the anchor covers the row.
  linkLabel = input<string>();
}
