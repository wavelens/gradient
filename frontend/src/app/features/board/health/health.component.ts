/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { BoardService, BoardHealth } from '@core/services/board.service';
import { AdminService, AdminTask } from '@core/services/admin.service';

const MIB = 1024 ** 2;

@Component({
  selector: 'app-board-health',
  standalone: true,
  imports: [CommonModule, RouterModule],
  template: `
    @if (health(); as h) {
      @if (h.draining) {
        <div class="drain-banner">Instance is draining: scheduling is paused and in-flight evaluations are parked. Clears on restart.</div>
      }
      <div class="kpis">
        <div class="kpi"><span class="label">Version</span><span class="value sm">{{ h.version }}</span></div>
        <div class="kpi"><span class="label">Uptime</span><span class="value sm">{{ uptime(h.uptime_seconds) }}</span></div>
        <div class="kpi"><span class="label">Workers</span><span class="value">{{ h.workers_connected }}</span></div>
        <div class="kpi"><span class="label">Jobs pending / active</span><span class="value sm">{{ h.jobs_pending }} / {{ h.jobs_active }}</span></div>
        <div class="kpi"><span class="label">Sessions</span><span class="value">{{ h.proto_sessions }}</span></div>
      </div>

      <h2>Process</h2>
      <div class="grid">
        <div class="cell"><span class="label">RSS</span><span>{{ mib(h.process.resident_memory_bytes) }} MiB</span></div>
        <div class="cell"><span class="label">Virtual</span><span>{{ mib(h.process.virtual_memory_bytes) }} MiB</span></div>
        <div class="cell"><span class="label">Open fds</span><span>{{ h.process.open_fds }} / {{ h.process.max_fds }}</span></div>
        <div class="cell"><span class="label">Threads</span><span>{{ h.process.threads }}</span></div>
        <div class="cell"><span class="label">CPU seconds</span><span>{{ h.process.cpu_seconds_total | number: '1.0-0' }}</span></div>
      </div>

      <h2>Pipeline</h2>
      <div class="grid">
        <div class="cell"><span class="label">Rollup lag</span><span [class.bad]="(h.rollup_lag_seconds ?? 0) > 300">{{ h.rollup_lag_seconds !== null ? (h.rollup_lag_seconds | number: '1.0-0') + ' s' : 'no data' }}</span></div>
        <div class="cell"><span class="label">Latest bucket</span><span>{{ h.latest_rollup_bucket ? (h.latest_rollup_bucket | date: 'short') : '-' }}</span></div>
        <div class="cell"><span class="label">Cache size</span><span>{{ (h.cache_bytes / (1024*1024*1024)) | number: '1.2-2' }} GiB</span></div>
        <div class="cell"><span class="label">Packages</span><span>{{ h.cache_packages }}</span></div>
      </div>

      <h2>Supervision</h2>
      <table class="http supervision">
        <thead><tr><th>Loop</th><th>Restarts</th><th>Errors</th><th>Timeouts</th><th>Last ok</th><th>Last error</th></tr></thead>
        <tbody>
          @for (l of h.supervised; track l.name) {
            <tr>
              <td>{{ l.name }}</td>
              <td [class.bad]="l.restarts > 0">{{ l.restarts }}</td>
              <td [class.bad]="l.pass_errors > 0">{{ l.pass_errors }}</td>
              <td [class.bad]="l.pass_timeouts > 0">{{ l.pass_timeouts }}</td>
              <td>{{ l.last_ok_seconds_ago !== null ? (l.last_ok_seconds_ago | number: '1.0-0') + ' s ago' : 'never' }}</td>
              <td [class.bad]="!!l.last_error">{{ l.last_error ?? '' }}</td>
            </tr>
          } @empty {
            <tr><td colspan="6" class="muted">No supervised loops reported.</td></tr>
          }
        </tbody>
      </table>

      <h2>Admin</h2>
      <div class="admin-actions">
        @if (!githubConfigured()) {
          <a class="btn" routerLink="/admin/github-app">Set up GitHub App</a>
        }
        <button class="btn" (click)="runDeepGc()" [disabled]="gcBusy()">Run Deep GC</button>
        <button class="btn" [class.danger]="!h.draining" (click)="toggleDraining(h.draining)" [disabled]="drainBusy()">
          {{ h.draining ? 'Disable Draining' : 'Enable Draining' }}
        </button>
        @if (gcNotice(); as n) { <span class="notice">{{ n }}</span> }
      </div>

      <table class="http">
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
      </table>
    } @else {
      <p class="muted">Loading… (superuser only)</p>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './health.component.scss',
})
export class BoardHealthComponent implements OnInit {
  private board = inject(BoardService);
  private admin = inject(AdminService);

  health = signal<BoardHealth | null>(null);
  tasks = signal<AdminTask[]>([]);
  githubConfigured = signal(false);
  gcBusy = signal(false);
  gcNotice = signal<string | null>(null);
  drainBusy = signal(false);

  mib(bytes: number): string {
    return (bytes / MIB).toFixed(0);
  }

  uptime(seconds: number): string {
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    return h > 0 ? `${h}h ${m}m` : `${m}m`;
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
    this.admin.githubAppConfigured().subscribe((v) => this.githubConfigured.set(v));
  }
}
