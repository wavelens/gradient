/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import { HistoryBar } from '@core/models';
import { PHASE_COLOR, evaluationPhase } from '@shared/evaluation';
import { formatDuration } from '@shared/text';
import { TooltipDirective } from '@gradient/ui/ui';
import { links } from '@shared/routing';

const MIN_HEIGHT_PCT = 15;

@Component({
  selector: 'app-evaluation-history',
  standalone: true,
  imports: [RouterLink, TooltipDirective],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <div class="history">
      @for (b of view(); track b.id) {
        <a
          class="bar"
          [style.background]="b.color"
          [style.height]="b.height"
          [routerLink]="links.task(project(), task(), b.id)"
          [grTooltip]="b.title"
          [attr.aria-label]="b.title"
        ></a>
      }
    </div>
  `,
  styleUrl: './evaluation-history.component.scss',
})
export class EvaluationHistoryComponent {
  protected readonly links = links;
  bars = input.required<HistoryBar[]>();
  project = input.required<string>();
  task = input.required<string>();

  view = computed(() => {
    const bars = this.bars();
    const max = Math.max(1, ...bars.map((b) => b.duration_ms ?? 0));
    return bars.map((b) => ({
      id: b.id,
      color: PHASE_COLOR[evaluationPhase(b.status)],
      height: `${Math.max(MIN_HEIGHT_PCT, Math.round(((b.duration_ms ?? max) / max) * 100))}%`,
      title: `${b.status} · ${formatDuration(b.duration_ms)}`,
    }));
  });
}
