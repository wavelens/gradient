/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import {
  BoardService,
  DispatchedJobDetail,
  GradientCapabilities,
  InstanceContextView,
  PendingJobSummary,
  Windowed,
} from '@core/services/board.service';
import { EvaluationsService, BuildWithOutputs } from '@core/services/evaluations.service';

interface RuleRow {
  name: string;
  value: number;
  share: number;
  positive: boolean;
}

@Component({
  selector: 'app-board-job-detail',
  standalone: true,
  imports: [CommonModule, RouterModule, DialogModule, ButtonModule],
  template: `
    <a routerLink="/board/live" class="back">← Live Jobs</a>

    @if (job(); as j) {
      <header class="head">
        <div>
          <span class="kind" [class.build]="j.kind === 1">{{ j.kind === 1 ? 'build' : 'eval' }}</span>
          <h1>Dispatched job</h1>
        </div>
        <div class="total">
          <span class="label">Total score</span>
          <span class="value">{{ j.score | number: '1.2-2' }}</span>
        </div>
      </header>

      <section class="ids">
        <div>
          <span class="label">Worker</span>
          <a class="mono worker-link" [routerLink]="['/organization', j.organization_name, 'workers', j.worker_id, 'metrics']">{{ j.worker_id }}</a>
        </div>
        <div><span class="label">Evaluation</span><span class="mono">{{ j.evaluation_id }}</span></div>
      </section>

      <section class="timeline">
        <div class="step"><span class="label">Queued</span><span>{{ j.queued_at | date: 'medium' }}</span></div>
        <div class="step"><span class="label">Wait</span><span class="hl">{{ waitLabel() }}</span></div>
        <div class="step"><span class="label">Dispatched</span><span>{{ j.dispatched_at | date: 'medium' }}</span></div>
        <div class="step"><span class="label">Current State</span><span class="hl">{{ currentState() }}</span></div>
      </section>

      <h2>Score breakdown</h2>
      <table class="rules">
        <thead><tr><th>Rule</th><th class="num">Contribution</th><th>Share</th></tr></thead>
        <tbody>
          @for (r of rules(); track r.name) {
            <tr>
              <td class="mono">{{ r.name }}</td>
              <td class="num" [class.neg]="!r.positive">{{ r.value | number: '1.2-2' }}</td>
              <td class="bar-cell">
                <div class="bar" [class.neg]="!r.positive" [style.width.%]="r.share"></div>
              </td>
            </tr>
          } @empty {
            <tr><td colspan="3" class="muted">No per-rule breakdown recorded.</td></tr>
          }
        </tbody>
      </table>

      @if (j.candidates) {
        <h2>Runner-up candidates</h2>
        <pre>{{ j.candidates | json }}</pre>
      }

      <section class="ctx worker">
        <h2>Worker context</h2>
        <table class="kv">
          <tbody>
            <tr><td class="label">Architectures</td><td class="mono">{{ j.worker_context.architectures.join(', ') || '—' }}</td></tr>
            <tr><td class="label">System features</td><td class="mono">{{ j.worker_context.system_features.join(', ') || '—' }}</td></tr>
            <tr><td class="label">Capabilities</td><td class="mono">{{ capabilityList(j.worker_context.capabilities) || '—' }}</td></tr>
            <tr><td class="label">CPU count</td><td class="mono">{{ j.worker_context.cpu_count }}</td></tr>
            <tr><td class="label">CPU core score</td><td class="mono">{{ j.worker_context.cpu_core_score | number: '1.0-2' }}</td></tr>
            <tr><td class="label">CPU usage</td><td class="mono">{{ j.worker_context.cpu_usage_pct | number: '1.0-1' }} %</td></tr>
            <tr><td class="label">RAM total</td><td class="mono">{{ j.worker_context.ram_total_mb | number }} MB</td></tr>
            <tr><td class="label">RAM free</td><td class="mono">{{ j.worker_context.ram_free_mb | number }} MB</td></tr>
            <tr><td class="label">Disk speed</td><td class="mono">{{ j.worker_context.disk_speed_mbps != null ? (j.worker_context.disk_speed_mbps | number) + ' MB/s' : '—' }}</td></tr>
            <tr><td class="label">Network speed</td><td class="mono">{{ j.worker_context.network_speed_mbps != null ? (j.worker_context.network_speed_mbps | number) + ' Mbps' : '—' }}</td></tr>
          </tbody>
        </table>
      </section>

      <section class="ctx job">
        <h2>Job context</h2>
        <table class="kv">
          <tbody>
            <tr><td class="label">Kind</td><td class="mono">{{ j.job_context.kind }}</td></tr>
            @if (j.job_context.kind === 'Build') {
              <tr><td class="label">Architecture</td><td class="mono">{{ j.job_context.architecture }}</td></tr>
            }
            <tr><td class="label">Missing count</td><td class="mono">{{ j.job_context.missing_count ?? '—' }}</td></tr>
            <tr><td class="label">Missing NAR size</td><td class="mono">{{ j.job_context.missing_nar_size != null ? (j.job_context.missing_nar_size | number) : '—' }}</td></tr>
            <tr><td class="label">Org work share</td><td class="mono">{{ j.job_context.org_work_share != null ? (j.job_context.org_work_share | number: '1.0-3') : '—' }}</td></tr>
            <tr><td class="label">Rescore count</td><td class="mono">{{ j.job_context.rescore_count }}</td></tr>
            <tr><td class="label">Queued</td><td class="mono">{{ j.job_context.queued_at | date: 'medium' }}</td></tr>
            <tr><td class="label">Ready</td><td class="mono">{{ j.job_context.ready_at | date: 'medium' }}</td></tr>
            @if (j.job_context.kind === 'Build') {
              <tr><td class="label">Dependency count</td><td class="mono">{{ j.job_context.dependency_count ?? '—' }}</td></tr>
              <tr><td class="label">Package name</td><td class="mono">{{ j.job_context.pname ?? '—' }}</td></tr>
              <tr><td class="label">Closure size</td><td class="mono">{{ j.job_context.closure_size != null ? (j.job_context.closure_size | number) : '—' }}</td></tr>
              <tr><td class="label">Prefer local build</td><td class="mono">{{ j.job_context.prefer_local_build ? 'yes' : 'no' }}</td></tr>
              <tr><td class="label">Fixed output</td><td class="mono">{{ j.job_context.is_fixed_output ? 'yes' : 'no' }}</td></tr>
            } @else {
              <tr><td class="label">Fetch flake</td><td class="mono">{{ j.job_context.fetch_flake ? 'yes' : 'no' }}</td></tr>
            }
          </tbody>
        </table>

        @if (j.job_context.kind === 'Build' && j.job_context.history; as h) {
          <h3>History</h3>
          <table class="kv">
            <tbody>
              <tr><td class="label">Peak RAM</td><td class="mono">{{ h.peak_ram_mb | number }} MB</td></tr>
              <tr><td class="label">Avg CPU time</td><td class="mono">{{ h.avg_cpu_time_ms | number }} ms</td></tr>
              <tr><td class="label">Build time</td><td class="mono">{{ h.build_time_ms | number }} ms</td></tr>
              <tr><td class="label">Avg disk bytes</td><td class="mono">{{ h.avg_disk_bytes | number }}</td></tr>
              <tr><td class="label">OOM rate</td><td class="mono">{{ h.oom_rate | number: '1.0-3' }}</td></tr>
              <tr><td class="label">Samples</td><td class="mono">{{ h.samples }}</td></tr>
            </tbody>
          </table>
        }

        @if (j.job_context.derivations?.length) {
          <h3>Derivations</h3>
          <div class="drv-list">
            @for (d of j.job_context.derivations; track d.drv_path) {
              <div class="drv-row clickable" (click)="openBuild(d.build_id)">
                <span class="mono pname">{{ d.pname ?? '—' }}</span>
                <span class="mono path">{{ d.drv_path }}</span>
              </div>
            }
          </div>
        }
      </section>

      @if (j.instance_context; as inst) {
        <section class="ctx instance">
          <h2>Instance context</h2>
          <table class="rules">
            <thead><tr><th>Metric</th><th class="num">5m</th><th class="num">1h</th><th class="num">24h</th></tr></thead>
            <tbody>
              @for (m of instanceWindows(inst); track m.name) {
                <tr>
                  <td class="mono">{{ m.name }}</td>
                  <td class="num">{{ m.w.w5m | number: '1.0-2' }}</td>
                  <td class="num">{{ m.w.w1h | number: '1.0-2' }}</td>
                  <td class="num">{{ m.w.w24h | number: '1.0-2' }}</td>
                </tr>
              }
            </tbody>
          </table>
          <table class="kv counts">
            <tbody>
              <tr><td class="label">Active builds</td><td class="mono">{{ inst.active_builds }}</td></tr>
              <tr><td class="label">Pending builds</td><td class="mono">{{ inst.pending_builds }}</td></tr>
              <tr><td class="label">Total workers</td><td class="mono">{{ inst.total_workers }}</td></tr>
              <tr><td class="label">Idle workers</td><td class="mono">{{ inst.idle_workers }}</td></tr>
            </tbody>
          </table>
        </section>
      }
    } @else if (pending(); as p) {
      <header class="head">
        <div>
          <span class="kind" [class.build]="p.kind === 1">{{ p.kind === 1 ? 'build' : 'eval' }}</span>
          <h1>Pending job</h1>
        </div>
      </header>
      <p class="muted">Still queued — limited details are available until it is dispatched.</p>
      <section class="ids">
        <div><span class="label">Evaluation</span><span class="mono">{{ p.evaluation_id }}</span></div>
        @if (p.build_id) {
          <div><span class="label">Build</span><span class="mono">{{ p.build_id }}</span></div>
        }
      </section>
      <section class="timeline">
        <div class="step"><span class="label">Queued</span><span>{{ p.queued_at | date: 'medium' }}</span></div>
        <div class="step"><span class="label">Dependencies</span><span class="hl">{{ p.dependency_count }}</span></div>
      </section>
    } @else if (notFound()) {
      <p class="muted">Job not found.</p>
    } @else {
      <p class="muted">Loading job…</p>
    }

    <p-dialog
      header="Build info"
      [visible]="buildDialog()"
      (visibleChange)="buildDialog.set($event)"
      [modal]="true"
      [style]="{ width: '640px' }"
      [draggable]="false"
      [resizable]="false"
    >
      @if (buildLoading()) {
        <p class="muted">Loading build…</p>
      } @else if (buildError()) {
        <p class="muted">{{ buildError() }}</p>
      } @else if (build(); as b) {
        <div class="build-grid">
          <div><span class="label">Build ID</span><span class="mono">{{ b.id }}</span></div>
          <div><span class="label">Status</span><span class="mono">{{ b.status }}</span></div>
          <div><span class="label">Architecture</span><span class="mono">{{ b.architecture }}</span></div>
          <div><span class="label">Worker</span><span class="mono">{{ b.worker ?? '—' }}</span></div>
          @if (b.via) {
            <div><span class="label">Via</span><span class="mono">{{ b.via }}</span></div>
          }
          <div class="span2"><span class="label">Derivation</span><span class="mono">{{ b.derivation_path }}</span></div>
          <div><span class="label">Created</span><span>{{ b.created_at | date: 'medium' }}</span></div>
          <div><span class="label">Updated</span><span>{{ b.updated_at | date: 'medium' }}</span></div>
          @if (outputs(b).length) {
            <div class="span2">
              <span class="label">Outputs</span>
              @for (o of outputs(b); track o.name) {
                <div class="mono out">{{ o.name }}: {{ o.path }}</div>
              }
            </div>
          }
        </div>
      }
      <ng-template pTemplate="footer">
        <button pButton label="Close" severity="secondary" (click)="buildDialog.set(false)"></button>
      </ng-template>
    </p-dialog>
  `,
  styles: [
    `
      :host { display: block; padding: 1.5rem; max-width: 1000px; margin: 0 auto; }
      .back { color: #abb0b4; text-decoration: none; font-size: 0.85rem; }
      .head { display: flex; justify-content: space-between; align-items: flex-end; margin: 0.5rem 0 1.5rem; }
      h1 { color: #fff; font-size: 1.4rem; margin: 0.25rem 0 0; }
      h2 { color: #fff; font-size: 1.05rem; margin: 1.75rem 0 0.75rem; }
      .kind { display: inline-block; font-size: 0.7rem; text-transform: uppercase; letter-spacing: 0.05em; color: #17a2b8; background: #17a2b822; padding: 0.15rem 0.5rem; border-radius: 4px; }
      .kind.build { color: #fd7e14; background: #fd7e1422; }
      .total { text-align: right; }
      .total .value { display: block; color: #fff; font-size: 2rem; font-weight: 600; }
      .label { color: #818181; font-size: 0.75rem; display: block; }
      .ids { display: flex; gap: 2rem; flex-wrap: wrap; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 1rem; }
      .timeline { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 1rem; margin-top: 1rem; }
      .step { background: #21262d; border: 1px solid #2d333b; border-radius: 8px; padding: 0.75rem; color: #abb0b4; font-size: 0.85rem; }
      .step .hl { color: #17a2b8; font-weight: 600; }
      .mono { font-family: monospace; color: #d6dade; font-size: 0.85rem; }
      .worker-link { text-decoration: none; cursor: pointer; }
      .worker-link:hover { color: #17a2b8; }
      .build-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
      .build-grid .span2 { grid-column: 1 / -1; }
      .build-grid .out { word-break: break-all; margin-top: 0.25rem; }
      table.rules { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; font-size: 0.85rem; color: #abb0b4; }
      th { color: #fff; }
      .num { text-align: right; font-variant-numeric: tabular-nums; color: #28a745; }
      .num.neg { color: #dc3545; }
      .bar-cell { width: 40%; }
      .bar { height: 10px; background: #28a745; border-radius: 3px; min-width: 2px; }
      .bar.neg { background: #dc3545; }
      pre { background: #0d1118; padding: 0.75rem; border-radius: 6px; overflow: auto; color: #abb0b4; font-size: 0.8rem; }
      .muted { color: #818181; }
      .ctx h3 { color: #fff; font-size: 0.95rem; margin: 1.25rem 0 0.5rem; }
      table.kv { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      table.kv td { padding: 0.4rem 0.75rem; border-bottom: 1px solid #2d333b; font-size: 0.85rem; }
      table.kv td.label { width: 40%; display: table-cell; }
      table.kv.counts { margin-top: 1rem; }
      .drv-list { display: flex; flex-direction: column; gap: 0.5rem; }
      .drv-row { display: flex; flex-direction: column; gap: 0.15rem; background: #21262d; border: 1px solid #2d333b; border-radius: 6px; padding: 0.5rem 0.75rem; }
      .drv-row.clickable { cursor: pointer; transition: background 0.1s, border-color 0.1s; }
      .drv-row.clickable:hover { background: #2d333b; border-color: #444c56; }
      .drv-row .pname { color: #d6dade; }
      .drv-row .path { color: #818181; font-size: 0.8rem; word-break: break-all; }
    `,
  ],
})
export class BoardJobDetailComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private board = inject(BoardService);
  private evaluations = inject(EvaluationsService);

  job = signal<DispatchedJobDetail | null>(null);
  pending = signal<PendingJobSummary | null>(null);
  notFound = signal(false);
  build = signal<BuildWithOutputs | null>(null);
  buildDialog = signal(false);
  buildLoading = signal(false);
  buildError = signal<string | null>(null);

  private static readonly WORKER_CAPS: (keyof GradientCapabilities)[] = ['federate', 'fetch', 'eval', 'build'];

  currentState = computed(() => {
    const j = this.job();
    if (!j) return '—';
    const b = this.build();
    if (b && b.id === j.build_id) return b.status;

    return j.finished_at ? 'finished' : 'running';
  });

  rules = computed<RuleRow[]>(() => {
    const rules = this.job()?.score_breakdown?.rules ?? {};
    const entries = Object.entries(rules);
    const maxAbs = Math.max(1e-9, ...entries.map(([, v]) => Math.abs(v)));
    return entries
      .map(([name, value]) => ({
        name,
        value,
        share: (Math.abs(value) / maxAbs) * 100,
        positive: value >= 0,
      }))
      .sort((a, b) => Math.abs(b.value) - Math.abs(a.value));
  });

  waitLabel = computed(() => {
    const j = this.job();
    if (!j) return '—';
    const ms = new Date(j.dispatched_at).getTime() - new Date(j.queued_at).getTime();
    if (!isFinite(ms) || ms < 0) return '—';
    return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
  });

  ngOnInit(): void {
    const id = this.route.snapshot.paramMap.get('id');
    if (!id) return;
    this.board.getJob(id).subscribe({
      next: (d) => {
        this.job.set(d);
        if (d.build_id) {
          this.loadBuild(d.build_id);
        }
      },
      error: () => this.loadPending(id),
    });
  }

  private loadPending(id: string): void {
    this.board.getPendingJobs().subscribe({
      next: (r) => {
        const match = r.jobs.find((p) => p.evaluation_id === id || p.build_id === id);
        match ? this.pending.set(match) : this.notFound.set(true);
      },
      error: () => this.notFound.set(true),
    });
  }

  openBuild(buildId: string): void {
    this.buildDialog.set(true);
    if (this.build()?.id !== buildId) {
      this.loadBuild(buildId);
    }
  }

  outputs(b: BuildWithOutputs): { name: string; path: string }[] {
    return Object.entries(b.output ?? {}).map(([name, path]) => ({ name, path }));
  }

  capabilityList(c: GradientCapabilities): string {
    return BoardJobDetailComponent.WORKER_CAPS.filter((k) => c[k]).join(', ');
  }

  instanceWindows(inst: InstanceContextView): { name: string; w: Windowed }[] {
    return [
      { name: 'Wait (s)', w: inst.wait_secs },
      { name: 'Build time (ms)', w: inst.build_time_ms },
      { name: 'Peak RAM (MB)', w: inst.peak_ram_mb },
      { name: 'CPU time (ms)', w: inst.cpu_time_ms },
      { name: 'Avg CPU (%)', w: inst.avg_cpu_pct },
      { name: 'Disk bytes', w: inst.disk_bytes },
      { name: 'Network (Mbps)', w: inst.network_mbps },
      { name: 'OOM rate', w: inst.oom_rate },
      { name: 'Closure size', w: inst.closure_size },
      { name: 'NAR size (MB)', w: inst.nar_size_mb },
      { name: 'Missing paths', w: inst.missing_paths },
      { name: 'Dependency count', w: inst.dependency_cnt },
      { name: 'Completed', w: inst.completed },
    ];
  }

  private loadBuild(buildId: string): void {
    this.buildLoading.set(true);
    this.buildError.set(null);
    this.evaluations.getBuild(buildId).subscribe({
      next: (b) => {
        this.build.set(b);
        this.buildLoading.set(false);
      },
      error: () => {
        this.buildError.set('Failed to load build.');
        this.buildLoading.set(false);
      },
    });
  }
}
