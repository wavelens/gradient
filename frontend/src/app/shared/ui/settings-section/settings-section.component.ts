/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';
import { CommonModule } from '@angular/common';

@Component({
  selector: 'gr-settings-section',
  standalone: true,
  imports: [CommonModule],
  templateUrl: './settings-section.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './settings-section.component.scss',
})
export class SettingsSectionComponent {
  title = input<string>();
  description = input<string>();
  card = input(true, { transform: booleanAttribute });
  /// A destructive group: the heading and the card edge read as danger.
  danger = input(false, { transform: booleanAttribute });
  maxWidth = input<string>('640px');
}
