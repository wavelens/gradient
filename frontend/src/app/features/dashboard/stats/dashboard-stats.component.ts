/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { DashboardService } from '@core/services/dashboard.service';
import { DashboardStats } from '@core/models';
import { ButtonComponent, CardGridComponent, MessageBannerComponent, StatCardComponent } from '@shared/ui';
import { formatBytes, formatCount, formatDuration } from '@shared/text';
import { formatCpuTime } from '../format';

@Component({
  selector: 'app-dashboard-stats',
  standalone: true,
  imports: [ButtonComponent, CardGridComponent, MessageBannerComponent, StatCardComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @if (stats(); as s) {
      <gr-card-grid min="150px">
        <gr-stat-card compact label="CPU time" [value]="cpu(s.cpu_time_ms)" />
        <gr-stat-card compact label="Builds" [value]="count(s.builds_completed)" />
        <gr-stat-card compact label="Cache size" [value]="bytes(s.cache_size_bytes)" />
        <gr-stat-card compact label="Workers busy" [value]="s.workers.online ? s.workers.busy_pct + '%' : '-'" />
        <gr-stat-card compact label="Queue wait" [value]="wait(s.queue_wait_p50_ms)" />
      </gr-card-grid>
    } @else if (failed()) {
      <gr-message-banner type="error">
        Stats unavailable.
        <button grButton size="small" [text]="true" label="Retry" (click)="load()"></button>
      </gr-message-banner>
    }
  `,
})
export class DashboardStatsComponent implements OnInit {
  private dashboard = inject(DashboardService);
  stats = signal<DashboardStats | null>(null);
  failed = signal(false);

  readonly cpu = formatCpuTime;
  readonly count = formatCount;
  readonly bytes = formatBytes;
  readonly wait = formatDuration;

  ngOnInit(): void {
    this.load();
  }

  load(): void {
    this.failed.set(false);
    this.dashboard.stats().subscribe({
      next: (s) => this.stats.set(s),
      error: (e: { status?: number }) => this.failed.set(e?.status !== 403),
    });
  }
}
