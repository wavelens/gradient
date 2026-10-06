/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, computed, inject, signal } from '@angular/core';
import { DashboardService } from '@core/services/dashboard.service';
import { DashboardStats } from '@core/models';
import {
  ButtonComponent,
  CardGridComponent,
  MessageBannerComponent,
  StatCardComponent,
} from '@gradient/ui/ui';
import { formatBytes, formatCount, formatDuration } from '@shared/text';
import { formatCpuTime } from '../format';

@Component({
  selector: 'app-dashboard-stats',
  standalone: true,
  imports: [ButtonComponent, CardGridComponent, MessageBannerComponent, StatCardComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @if (failed()) {
      <gr-message-banner type="error">
        Stats unavailable.
        <button grButton size="small" [text]="true" label="Retry" (click)="load()"></button>
      </gr-message-banner>
    } @else if (!hidden()) {
      <gr-card-grid min="150px" [attr.aria-busy]="!stats()">
        @for (c of cards(); track c.label) {
          <gr-stat-card compact [label]="c.label" [value]="c.value" />
        }
      </gr-card-grid>
    }
  `,
  styleUrl: './dashboard-stats.component.scss',
})
export class DashboardStatsComponent implements OnInit {
  private dashboard = inject(DashboardService);
  stats = signal<DashboardStats | null>(null);
  failed = signal(false);
  hidden = signal(false);

  cards = computed(() => {
    const s = this.stats();
    return [
      { label: 'CPU time', value: s && formatCpuTime(s.cpu_time_ms) },
      { label: 'Builds', value: s && formatCount(s.builds_completed) },
      { label: 'Cache size', value: s && formatBytes(s.cache_size_bytes) },
      { label: 'Workers busy', value: s && (s.workers.online ? `${s.workers.busy_pct}%` : '-') },
      { label: 'Avg. Queue wait', value: s && formatDuration(s.queue_wait_p50_ms) },
    ];
  });

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.failed.set(false);
    this.dashboard.stats().subscribe({
      next: (s) => this.stats.set(s),
      error: (e: { status?: number }) => {
        this.hidden.set(e?.status === 403);
        this.failed.set(e?.status !== 403);
      },
    });
  }
}
