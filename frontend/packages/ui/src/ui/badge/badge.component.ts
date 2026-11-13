/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';
import { BadgeSeverity } from '../types';

@Component({
  selector: 'gr-badge',
  standalone: true,
  template: `<span class="badge" [class]="'is-' + severity()">@if (showStatusDot()) {<span class="badge__status-dot"></span>}<ng-content></ng-content></span>`,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './badge.component.scss',
})
export class BadgeComponent {
  severity = input<BadgeSeverity>('neutral');
  showStatusDot = input(false, { transform: booleanAttribute });
}
