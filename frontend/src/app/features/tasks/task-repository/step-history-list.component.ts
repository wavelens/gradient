/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { BadgeComponent, type BadgeSeverity } from '@gradient/ui/ui';
import { RelativeTimePipe, ServerTimestampToLocalDateTimePipe } from '@shared/text';

export interface StepHistoryLine {
  id: string;
  label: string;
  severity: BadgeSeverity;
  text: string;
  at: string;
}

@Component({
  selector: 'app-step-history-list',
  standalone: true,
  imports: [BadgeComponent, RelativeTimePipe, ServerTimestampToLocalDateTimePipe],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <ul>
      @for (line of lines(); track line.id) {
        <li>
          <gr-badge [severity]="line.severity">{{ line.label }}</gr-badge>
          <span class="history-text">{{ line.text }}</span>
          <span class="text-secondary history-time" [title]="line.at | serverDate">{{ line.at | relativeTime }}</span>
        </li>
      }
    </ul>
  `,
  styles: `
    @use '@gradient/ui/styles/variables' as *;

    :host { display: block; }
    ul { list-style: none; margin: 0; padding: 0; }
    li { display: flex; flex-wrap: wrap; align-items: baseline; gap: $spacing-xs $spacing-sm; padding: $spacing-xs 0; }
    .history-text { flex: 1 1 12rem; min-width: 0; overflow-wrap: anywhere; }
    .history-time { white-space: nowrap; }
  `,
})
export class StepHistoryListComponent {
  lines = input.required<StepHistoryLine[]>();
}
