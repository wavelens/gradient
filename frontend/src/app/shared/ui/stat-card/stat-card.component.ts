/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy } from '@angular/core';
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

  /// A metric is meant to be read at a glance, so the value takes the largest
  /// step it fits on. Without this a timestamp wraps and the row of cards
  /// grows to match the longest one.
  protected valueScale = computed(() => {
    const length = String(this.value()).length;
    if (length <= 8) return 'is-xxl';
    return length <= 15 ? 'is-xl' : 'is-lg';
  });
}
