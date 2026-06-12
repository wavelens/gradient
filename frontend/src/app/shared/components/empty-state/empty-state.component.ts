/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, booleanAttribute, input, output } from '@angular/core';
import { CommonModule } from '@angular/common';

@Component({
  selector: 'app-empty-state',
  standalone: true,
  imports: [CommonModule],
  templateUrl: './empty-state.component.html',
  styleUrl: './empty-state.component.scss',
  host: { '[class.flat]': 'flat()' },
})
export class EmptyStateComponent {
  icon = input.required<string>();
  title = input.required<string>();
  message = input<string>();
  actionLabel = input<string>();
  /// Renders without the boxed background, for use inside panels.
  flat = input(false, { transform: booleanAttribute });
  actionClick = output<void>();

  onActionClick(): void {
    this.actionClick.emit();
  }
}
