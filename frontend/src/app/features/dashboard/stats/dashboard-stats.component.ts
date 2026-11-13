/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { Observable, tap } from 'rxjs';
import { DashboardService } from '@core/services/dashboard.service';
import { DashboardStats } from '@core/models';
import {
  CardGridComponent,
  StatCardComponent,
} from '@gradient/ui/ui';
import { formatBytes, formatCount, formatDuration } from '@shared/text';
import { formatCpuTime } from '../format';

@Component({
  selector: 'app-dashboard-stats',
  standalone: true,
  imports: [CardGridComponent, StatCardComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @if (!hidden() && !failed()) {
      <gr-card-grid min="150px" [attr.aria-busy]="!stats()">
        @for (c of cards(); track c.label) {
          <gr-stat-card compact [label]="c.label" [value]="c.value" [valueExplanation]="c.valueExplanation" />
        }
      </gr-card-grid>
    }
  `,
})
export class DashboardStatsComponent implements OnInit {
  private dashboard = inject(DashboardService);
  stats = signal<DashboardStats | null>(null);
  failed = signal(false);
  hidden = signal(false);

  cards = computed(() => {
    const s = this.stats();
    return [
      ...(s?.cpu_time_ms == null
        ? []
        : [
            {
              label: 'CPU time',
              value: formatCpuTime(s.cpu_time_ms),
              valueExplanation: 'CPU time for builds in viewable projects. Only build records that remain on the server feed the total (90 days by default).',
            },
          ]),
      {
        label: 'Completed builds',
        value: s && formatCount(s.builds_completed),
        valueExplanation:
          "Builds that ran to completion in projects you can view, within evaluations that still exist on the server. Builds from a binary cache don't count. A build that several evaluations reuse counts once.",
      },
      {
        label: 'Total cached',
        value: s && formatBytes(s.cache_size_bytes),
        valueExplanation:
          'Total size stored across all caches you created or can reach through a project membership. Superusers see the sum of every cache on the server. Two caches that hold the same store path both count its size toward the total.',
      },
      {
        label: 'Worker load',
        value: s && (s.workers.online ? `${s.workers.busy_pct}%` : '0 online'),
        valueExplanation: 'The average percentage of build slots in use across connected workers that serve projects that you can view.',
      },
      {
        label: 'Queue wait',
        value: s && formatDuration(s.queue_wait_p50_ms),
        valueExplanation:
          'Median time a job waited in the scheduler queue before the scheduler assigned it to a worker machine. Covers jobs assigned in the last 24 hours, for projects you can view.',
      },
    ];
  });

  ngOnInit(): void {
    this.load();
  }

  reloadSilentlyKeepingContentOnError(): Observable<DashboardStats> {
    return this.dashboard.stats().pipe(
      tap((s) => {
        this.stats.set(s);
        this.failed.set(false);
      }),
    );
  }

  load(): void {
    this.failed.set(false);
    this.reloadSilentlyKeepingContentOnError().subscribe({
      error: (e: { status?: number }) => {
        this.hidden.set(e?.status === 403);
        this.failed.set(e?.status !== 403);
      },
    });
  }
}
