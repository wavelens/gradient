/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import { EvaluationStatus, HistoryBar } from '@core/models';
import { formatDuration } from '@shared/text';

const TONE: Partial<Record<EvaluationStatus, string>> = { Completed: 'ok', Failed: 'fail', Aborted: 'fail' };
const MIN_HEIGHT_PCT = 15;

@Component({
  selector: 'app-evaluation-history',
  standalone: true,
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <div class="history">
      @for (b of view(); track b.id) {
        <a
          [class]="'bar bar--' + b.tone"
          [style.height]="b.height"
          [routerLink]="['/project', project(), 'task', task()]"
          [queryParams]="{ eval: b.id }"
          [title]="b.title"
          [attr.aria-label]="b.title"
        ></a>
      }
    </div>
  `,
  styleUrl: './evaluation-history.component.scss',
})
export class EvaluationHistoryComponent {
  bars = input.required<HistoryBar[]>();
  project = input.required<string>();
  task = input.required<string>();

  view = computed(() => {
    const bars = this.bars();
    const max = Math.max(1, ...bars.map((b) => b.duration_ms ?? 0));
    return bars.map((b) => ({
      id: b.id,
      tone: TONE[b.status] ?? 'run',
      height: `${Math.max(MIN_HEIGHT_PCT, Math.round(((b.duration_ms ?? max) / max) * 100))}%`,
      title: `${b.status} · ${formatDuration(b.duration_ms)}`,
    }));
  });
}
