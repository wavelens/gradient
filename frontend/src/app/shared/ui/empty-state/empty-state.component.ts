/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, booleanAttribute, input, output, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ButtonComponent } from '@shared/ui';

@Component({
  selector: 'gr-empty-state',
  standalone: true,
  imports: [CommonModule, ButtonComponent],
  templateUrl: './empty-state.component.html',
  styleUrl: './empty-state.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
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
