/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, booleanAttribute, computed, input, linkedSignal } from '@angular/core';
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
    <div class="history" (keydown)="onHistoryBarKeydown($event)">
      @for (b of view(); track b.id; let i = $index) {
        @if (barsAreLinks()) {
          <a
            class="bar"
            [style.background]="b.color"
            [style.height]="b.height"
            [tabindex]="i === evaluationHistoryBarIndexInTabOrder() ? 0 : -1"
            [routerLink]="links.task(project(), task(), b.id)"
            [grTooltip]="b.title"
            [attr.aria-label]="b.title"
          ></a>
        } @else {
          <span class="bar" [style.background]="b.color" [style.height]="b.height" [title]="b.title"></span>
        }
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
  barsAreLinks = input(true, { transform: booleanAttribute });

  protected evaluationHistoryBarIndexInTabOrder = linkedSignal<HistoryBar[], number>({
    source: this.bars,
    computation: (bars, previous) => Math.min(previous?.value ?? bars.length - 1, bars.length - 1),
  });

  protected onHistoryBarKeydown(event: KeyboardEvent): void {
    const last = this.bars().length - 1;
    const current = this.evaluationHistoryBarIndexInTabOrder();
    const next = { ArrowLeft: current - 1, ArrowRight: current + 1, Home: 0, End: last }[event.key];
    if (next === undefined || last < 0) return;
    event.preventDefault();
    const index = Math.max(0, Math.min(last, next));
    this.evaluationHistoryBarIndexInTabOrder.set(index);
    (event.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('a.bar')[index]?.focus();
  }

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
