/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterLink } from '@angular/router';
import { CardGridComponent, LoadStateComponent, TableComponent, StatCardComponent } from '@gradient/ui/ui';
import { BoardService } from '@core/services/board.service';
import { AuthService } from '@core/services/auth.service';
import { links } from '@shared/routing';
import { formatBytes, formatDuration, ServerTimestampToLocalDateTimePipe } from '@shared/text';
import { BoardHealth } from '@core/models';

@Component({
  selector: 'app-board-health',
  standalone: true,
  imports: [CommonModule, RouterLink, ServerTimestampToLocalDateTimePipe, CardGridComponent, StatCardComponent, TableComponent, LoadStateComponent],
  template: `
    <gr-load-state [loading]="healthLoading()" [error]="healthError()" [empty]="!health()"
                   loadingMessage="Loading system health..." errorTitle="Failed to load health" (retry)="refreshHealth()">
    @if (health(); as h) {
      <gr-card-grid class="kpis" min="180px">
        <gr-stat-card compact label="Version" [value]="h.version" />
        <gr-stat-card compact label="Uptime" [value]="seconds(h.uptime_seconds)" />
        <gr-stat-card compact label="Worker and cache connections" valueExplanation="The current number of connections open to the server over the Gradient protocol. Connected workers have one open connection each. Cache clients on this protocol have one too." [value]="h.proto_sessions" />
      </gr-card-grid>

      <h2>Process</h2>
      <div class="grid">
        <div class="cell"><span class="label">RSS</span><span>{{ bytes(h.process.resident_memory_bytes) }}</span></div>
        <div class="cell"><span class="label">Virtual</span><span>{{ bytes(h.process.virtual_memory_bytes) }}</span></div>
        <div class="cell"><span class="label">Open fds</span><span>{{ h.process.open_fds }} / {{ h.process.max_fds }}</span></div>
        <div class="cell"><span class="label">Threads</span><span>{{ h.process.threads }}</span></div>
        <div class="cell"><span class="label">CPU time</span><span>{{ seconds(h.process.cpu_seconds_total) }}</span></div>
      </div>

      <h2>Pipeline</h2>
      <div class="grid">
        <div class="cell"><span class="label">Rollup lag</span><span [class.bad]="(h.rollup_lag_seconds ?? 0) > 300">{{ h.rollup_lag_seconds != null ? seconds(h.rollup_lag_seconds) : 'no data' }}</span></div>
        <div class="cell"><span class="label">Latest bucket</span><span>{{ h.latest_rollup_bucket ? (h.latest_rollup_bucket | serverDate) : '-' }}</span></div>
        <div class="cell"><span class="label">Cache size</span><span>{{ bytes(h.cache_bytes) }}</span></div>
        <div class="cell"><span class="label">Packages</span><span>{{ h.cache_packages }}</span></div>
        <div class="cell"><span class="label">Notification outbox pending</span><span>{{ h.pending_deliveries }}</span></div>
        <div class="cell"><span class="label">Notification outbox: permanent failures</span><span [class.bad]="h.failed_deliveries > 0">{{ h.failed_deliveries }}</span></div>
      </div>

      <h2>Supervision</h2>
      <gr-table class="http supervision">
        <thead><tr><th>Loop</th><th>Restarts</th><th>Errors</th><th>Timeouts</th><th>Last ok</th><th>Last error</th></tr></thead>
        <tbody>
          @for (l of supervisedLoopsThatHaveRestartsErrorsOrTimeouts(); track l.name) {
            <tr>
              <td>{{ l.name }}</td>
              <td [class.bad]="l.restarts > 0">{{ l.restarts }}</td>
              <td [class.bad]="l.pass_errors > 0">{{ l.pass_errors }}</td>
              <td [class.bad]="l.pass_timeouts > 0">{{ l.pass_timeouts }}</td>
              <td>{{ l.last_ok_seconds_ago != null ? seconds(l.last_ok_seconds_ago) + ' ago' : 'never' }}</td>
              <td [class.bad]="!!l.last_error">{{ l.last_error ?? '' }}</td>
            </tr>
          } @empty {
            <tr><td colspan="6" class="muted">
              @if (h.supervised.length) {
                All {{ h.supervised.length }} supervised loops run with no restarts, errors or timeouts.
              } @else {
                No supervised loops reported.
              }
            </td></tr>
          }
        </tbody>
      </gr-table>

      @if (superuser()) {
        <p class="muted admin-note"><a [routerLink]="links.admin('overview')">Deep GC, instance draining, and maintenance tasks are in the Admin area.</a></p>
      }
    }
    </gr-load-state>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './health.component.scss',
})
export class BoardHealthComponent implements OnInit {
  private board = inject(BoardService);
  private auth = inject(AuthService);

  health = signal<BoardHealth | null>(null);
  healthLoading = signal(true);
  healthError = signal<unknown>(null);
  readonly supervisedLoopsThatHaveRestartsErrorsOrTimeouts = computed(() =>
    (this.health()?.supervised ?? []).filter((l) => l.restarts > 0 || l.pass_errors > 0 || l.pass_timeouts > 0 || !!l.last_error));

  readonly bytes = formatBytes;
  protected readonly links = links;
  protected readonly superuser = computed(() => this.auth.user()?.superuser === true);

  seconds(value: number): string {
    return formatDuration(value * 1000);
  }

  refreshHealth(): void {
    this.healthLoading.set(true);
    this.healthError.set(null);
    this.board.getHealth().subscribe({
      next: (h) => {
        this.health.set(h);
        this.healthLoading.set(false);
      },
      error: (error: unknown) => {
        this.healthError.set(error);
        this.healthLoading.set(false);
      },
    });
  }

  ngOnInit(): void {
    this.refreshHealth();
  }
}
