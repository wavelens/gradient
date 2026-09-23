/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { DashboardService } from '@core/services/dashboard.service';
import { ActivityDay } from '@core/models';
import { ButtonComponent, TooltipDirective } from '@shared/ui';

type Mode = 'evaluations' | 'failed';
const STEP = 13;
const LEVELS = 4;

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;

@Component({
  selector: 'app-dashboard-activity',
  standalone: true,
  imports: [ButtonComponent, TooltipDirective],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @if (!hidden()) {
      <section>
        <div class="head">
          <h2>Activity</h2>
          @if (days(); as d) {
            <span class="hint">{{ total() }} {{ mode() === 'evaluations' ? '' : 'failed ' }}evaluation{{ total() === 1 ? '' : 's' }} in the last year</span>
          }
          <div class="seg" role="group" aria-label="Activity measure">
            <button type="button" [class.on]="mode() === 'evaluations'" [attr.aria-pressed]="mode() === 'evaluations'" (click)="mode.set('evaluations')">Evaluations</button>
            <button type="button" [class.on]="mode() === 'failed'" [attr.aria-pressed]="mode() === 'failed'" (click)="mode.set('failed')">Failures</button>
          </div>
        </div>
        @if (failed()) {
          <p class="error">
            Activity unavailable.
            <button grButton size="small" [text]="true" label="Retry" (click)="load()"></button>
          </p>
        } @else if (days()) {
          <svg
            role="img"
            [attr.aria-label]="summary()"
            [attr.viewBox]="'0 0 ' + width() + ' ' + 7 * STEP"
            class="heat"
            [class.heat--failed]="mode() === 'failed'"
          >
            @for (c of cells(); track c.date) {
              <rect
                [attr.class]="'day day--' + c.level"
                [attr.x]="c.x"
                [attr.y]="c.y"
                width="11"
                height="11"
                rx="2"
                [grTooltip]="c.title"
              ></rect>
            }
          </svg>
        }
      </section>
    }
  `,
  styleUrl: './dashboard-activity.component.scss',
})
export class DashboardActivityComponent implements OnInit {
  private dashboard = inject(DashboardService);
  readonly STEP = STEP;
  days = signal<ActivityDay[] | null>(null);
  mode = signal<Mode>('evaluations');
  failed = signal(false);
  hidden = signal(false);

  total = computed(() => (this.days() ?? []).reduce((a, d) => a + d[this.mode()], 0));
  summary = computed(() => {
    const key = this.mode();
    const days = this.days() ?? [];
    const what = key === 'evaluations' ? 'evaluations' : 'failed evaluations';
    const busiest = days.reduce<ActivityDay | null>((top, d) => (d[key] > (top?.[key] ?? 0) ? d : top), null);
    const head = `Daily ${what} over the last ${days.length} days`;
    return busiest ? `${head}: ${this.total()} in total, busiest day ${busiest.date} with ${busiest[key]}` : `${head}: none`;
  });
  private offset = computed(() => {
    const first = this.days()?.[0];
    return first ? new Date(`${first.date}T00:00:00Z`).getUTCDay() : 0;
  });
  width = computed(() => Math.ceil(((this.days()?.length ?? 0) + this.offset()) / 7) * STEP);

  cells = computed(() => {
    const key = this.mode();
    const days = this.days() ?? [];
    const max = Math.max(1, ...days.map((d) => d[key]));
    return days.map((d, i) => {
      const idx = i + this.offset();
      const v = d[key];
      return {
        date: d.date,
        x: Math.floor(idx / 7) * STEP,
        y: (idx % 7) * STEP,
        level: v === 0 ? 0 : Math.min(LEVELS, 1 + Math.floor((v / max) * LEVELS)),
        title: `${d.date}: ${plural(d.evaluations, 'evaluation')}, ${d.failed} failed`,
      };
    });
  });

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.failed.set(false);
    this.dashboard.activity().subscribe({
      next: (a) => this.days.set(a.days),
      error: (e: { status?: number }) => {
        this.hidden.set(e?.status === 403);
        this.failed.set(e?.status !== 403);
      },
    });
  }
}
