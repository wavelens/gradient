/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy, booleanAttribute } from '@angular/core';
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
  /// The measure keeps form fields readable. A card is a form, so it is measured
  /// by default; a bare section usually holds a list, which measures itself.
  maxWidth = input<string>();

  protected measure = computed(() => this.maxWidth() ?? (this.card() ? '640px' : null));
}
