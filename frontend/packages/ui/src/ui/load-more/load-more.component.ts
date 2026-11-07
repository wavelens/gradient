/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, input, output } from '@angular/core';
import { ButtonComponent } from '../button/button.component';

@Component({
  selector: 'gr-load-more',
  standalone: true,
  imports: [ButtonComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @if (shown() < total()) {
      <div class="gr-load-more">
        <span>Showing {{ shown() }} of {{ total() }}</span>
        <button grButton severity="secondary" icon="expand_more" label="Load more" [loading]="loading()" (click)="more.emit()"></button>
      </div>
    }
  `,
  styles: `
    @use '../../styles/variables' as *;

    .gr-load-more {
      display: flex;
      align-items: center;
      justify-content: center;
      gap: $spacing-md;
      margin-top: $spacing-lg;
      color: var(--gr-text-secondary);
      font-size: $font-size-sm;
    }
  `,
})
export class LoadMoreComponent {
  shown = input.required<number>();
  total = input.required<number>();
  loading = input(false, { transform: booleanAttribute });
  more = output();
}
