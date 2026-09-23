/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, OnInit, inject, signal } from '@angular/core';
import { DashboardService } from '@core/services/dashboard.service';
import { DashboardStats } from '@core/models';
import { ButtonComponent } from '@shared/ui';
import { formatBytes, formatCount, formatDuration } from '@shared/text';
import { formatCpuTime } from '../format';

@Component({
  selector: 'app-dashboard-stats',
  standalone: true,
  imports: [ButtonComponent],
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    @if (stats(); as s) {
      <div class="strip">
        <span><b>{{ cpu(s.cpu_time_ms) }}</b> CPU time</span>
        <span><b>{{ count(s.builds_completed) }}</b> builds</span>
        <span><b>{{ bytes(s.cache_size_bytes) }}</b> cache size</span>
        <span><b>{{ s.workers.busy_pct }}%</b> workers busy</span>
        <span><b>{{ wait(s.queue_wait_p50_ms) }}</b> queue wait</span>
      </div>
    } @else if (failed()) {
      <p class="error">
        Stats unavailable.
        <button grButton size="small" [text]="true" label="Retry" (click)="load()"></button>
      </p>
    }
  `,
  styleUrl: './dashboard-stats.component.scss',
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
