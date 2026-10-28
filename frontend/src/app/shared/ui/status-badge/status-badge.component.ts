/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';
import type { StatusPhase } from '@shared/evaluation';
import { StatusIconComponent } from '../status-icon/status-icon.component';

@Component({
  selector: 'gr-status-badge',
  standalone: true,
  imports: [StatusIconComponent],
  template: `
    <span class="status-badge" [attr.data-phase]="phase()">
      <gr-status-icon [phase]="phase()" [prioritized]="prioritized()" size="sm" />
      {{ label() }}
    </span>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './status-badge.component.scss',
})
export class StatusBadgeComponent {
  phase = input.required<StatusPhase>();
  label = input.required<string>();
  prioritized = input(false);
}
