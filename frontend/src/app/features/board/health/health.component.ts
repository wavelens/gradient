/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { LoadingSpinnerComponent, TableComponent } from '@shared/ui';
import { BoardService, BoardHealth } from '@core/services/board.service';
import { AdminService, AdminTask } from '@core/services/admin.service';
import { ConfigService } from '@core/services/config.service';
import { formatBytes, formatDuration } from '@shared/text';

@Component({
  selector: 'app-board-health',
  standalone: true,
  imports: [CommonModule, RouterModule, TableComponent, LoadingSpinnerComponent],
  template: `
    @if (health(); as h) {
      @if (h.draining) {
        <div class="drain-banner">Instance is draining: scheduling is paused and in-flight evaluations are parked. Clears on restart.</div>
      }
      <div class="kpis">
        <div class="kpi"><span class="label">Version</span><span class="value sm">{{ h.version }}</span></div>
        <div class="kpi"><span class="label">Uptime</span><span class="value sm">{{ seconds(h.uptime_seconds) }}</span></div>
        <div class="kpi"><span class="label">Workers</span><span class="value">{{ h.workers_connected }}</span></div>
        <div class="kpi"><span class="label">Jobs pending / active</span><span class="value sm">{{ h.jobs_pending }} / {{ h.jobs_active }}</span></div>
        <div class="kpi"><span class="label">Sessions</span><span class="value">{{ h.proto_sessions }}</span></div>
      </div>

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
        <div class="cell"><span class="label">Rollup lag</span><span [class.bad]="(h.rollup_lag_seconds ?? 0) > 300">{{ h.rollup_lag_seconds !== null ? seconds(h.rollup_lag_seconds) : 'no data' }}</span></div>
        <div class="cell"><span class="label">Latest bucket</span><span>{{ h.latest_rollup_bucket ? (h.latest_rollup_bucket | date: 'short') : '-' }}</span></div>
        <div class="cell"><span class="label">Cache size</span><span>{{ bytes(h.cache_bytes) }}</span></div>
        <div class="cell"><span class="label">Packages</span><span>{{ h.cache_packages }}</span></div>
        <div class="cell"><span class="label">Effects pending</span><span>{{ h.outbox_pending }}</span></div>
        <div class="cell"><span class="label">Effects dead-lettered</span><span [class.bad]="h.outbox_failed > 0">{{ h.outbox_failed }}</span></div>
      </div>

      <h2>Supervision</h2>
      <gr-table class="http supervision">
        <thead><tr><th>Loop</th><th>Restarts</th><th>Errors</th><th>Timeouts</th><th>Last ok</th><th>Last error</th></tr></thead>
        <tbody>
          @for (l of h.supervised; track l.name) {
            <tr>
              <td>{{ l.name }}</td>
              <td [class.bad]="l.restarts > 0">{{ l.restarts }}</td>
              <td [class.bad]="l.pass_errors > 0">{{ l.pass_errors }}</td>
              <td [class.bad]="l.pass_timeouts > 0">{{ l.pass_timeouts }}</td>
              <td>{{ l.last_ok_seconds_ago !== null ? seconds(l.last_ok_seconds_ago) + ' ago' : 'never' }}</td>
              <td [class.bad]="!!l.last_error">{{ l.last_error ?? '' }}</td>
            </tr>
          } @empty {
            <tr><td colspan="6" class="muted">No supervised loops reported.</td></tr>
          }
        </tbody>
      </gr-table>

      <h2>Admin</h2>
      <div class="admin-actions">
        @if (!config.githubAppEnabled) {
          <a class="btn" routerLink="/admin/github-app">Set up GitHub App</a>
        }
        <button class="btn" (click)="runDeepGc()" [disabled]="gcBusy()">Run Deep GC</button>
        <button class="btn" [class.danger]="!h.draining" (click)="toggleDraining(h.draining)" [disabled]="drainBusy()">
          {{ h.draining ? 'Disable Draining' : 'Enable Draining' }}
        </button>
        @if (gcNotice(); as n) { <span class="notice">{{ n }}</span> }
      </div>

      <gr-table class="http">
        <thead><tr><th>Task</th><th>Status</th><th>Created</th><th>Finished</th><th>Error</th></tr></thead>
        <tbody>
          @for (t of tasks(); track t.id) {
            <tr>
              <td>{{ t.kind }}</td>
              <td>{{ t.status }}</td>
              <td>{{ t.created_at | date: 'short' }}</td>
              <td>{{ t.finished_at ? (t.finished_at | date: 'short') : '-' }}</td>
              <td [class.bad]="!!t.error">{{ t.error ?? '' }}</td>
            </tr>
          } @empty {
            <tr><td colspan="5" class="muted">No admin tasks yet.</td></tr>
          }
        </tbody>
      </gr-table>
    } @else {
      <gr-loading-spinner message="Loading system health..." />
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './health.component.scss',
})
export class BoardHealthComponent implements OnInit {
  private board = inject(BoardService);
  private admin = inject(AdminService);
  protected config = inject(ConfigService);

  health = signal<BoardHealth | null>(null);
  tasks = signal<AdminTask[]>([]);
  gcBusy = signal(false);
  gcNotice = signal<string | null>(null);
  drainBusy = signal(false);

  readonly bytes = formatBytes;

  seconds(value: number): string {
    return formatDuration(value * 1000);
  }

  private loadTasks(): void {
    this.admin.listTasks().subscribe((t) => this.tasks.set(t));
  }

  runDeepGc(): void {
    this.gcBusy.set(true);
    this.gcNotice.set(null);
    this.admin.startDeepGc().subscribe({
      next: () => { this.gcBusy.set(false); this.loadTasks(); },
      error: (e) => { this.gcBusy.set(false); this.gcNotice.set(e?.message ?? 'Deep GC failed to start'); },
    });
  }

  toggleDraining(current: boolean): void {
    this.drainBusy.set(true);
    this.admin.setDraining(!current).subscribe({
      next: () => { this.drainBusy.set(false); this.refreshHealth(); },
      error: () => this.drainBusy.set(false),
    });
  }

  private refreshHealth(): void {
    this.board.getHealth().subscribe((h) => this.health.set(h));
  }

  ngOnInit(): void {
    this.refreshHealth();
    this.loadTasks();
  }
}
