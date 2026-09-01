/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';
import { IconComponent } from '../icon/icon.component';
import { CommonModule } from '@angular/common';

@Component({
  selector: 'gr-stat-card',
  standalone: true,
  imports: [IconComponent, CommonModule],
  templateUrl: './stat-card.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './stat-card.component.scss',
})
export class StatCardComponent {
  icon = input<string>();
  value = input.required<number | string>();
  label = input.required<string>();

}
