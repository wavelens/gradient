/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, computed } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { BoardService, DispatchedJobDetail } from '@core/services/board.service';

interface RuleRow {
  name: string;
  value: number;
  share: number;
  positive: boolean;
}

@Component({
  selector: 'app-board-job-detail',
  standalone: true,
  imports: [CommonModule, RouterModule],
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
        <div><span class="label">Worker</span><span class="mono">{{ j.worker_id }}</span></div>
        <div><span class="label">Evaluation</span><span class="mono">{{ j.evaluation_id }}</span></div>
        @if (j.build_id) {
          <div><span class="label">Build</span><span class="mono">{{ j.build_id }}</span></div>
        }
      </section>

      <section class="timeline">
        <div class="step"><span class="label">Queued</span><span>{{ j.queued_at | date: 'medium' }}</span></div>
        <div class="step"><span class="label">Wait</span><span class="hl">{{ waitLabel() }}</span></div>
        <div class="step"><span class="label">Dispatched</span><span>{{ j.dispatched_at | date: 'medium' }}</span></div>
        <div class="step"><span class="label">Finished</span><span>{{ j.finished_at ? (j.finished_at | date: 'medium') : 'running' }}</span></div>
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

      <details>
        <summary>Job context</summary>
        <pre>{{ j.job_context | json }}</pre>
      </details>
      <details>
        <summary>Worker context</summary>
        <pre>{{ j.worker_context | json }}</pre>
      </details>
    } @else {
      <p class="muted">Loading job…</p>
    }
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
      table.rules { width: 100%; border-collapse: collapse; background: #21262d; border: 1px solid #2d333b; border-radius: 8px; overflow: hidden; }
      th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #2d333b; font-size: 0.85rem; color: #abb0b4; }
      th { color: #fff; }
      .num { text-align: right; font-variant-numeric: tabular-nums; color: #28a745; }
      .num.neg { color: #dc3545; }
      .bar-cell { width: 40%; }
      .bar { height: 10px; background: #28a745; border-radius: 3px; min-width: 2px; }
      .bar.neg { background: #dc3545; }
      details { margin-top: 1rem; color: #abb0b4; }
      pre { background: #0d1118; padding: 0.75rem; border-radius: 6px; overflow: auto; color: #abb0b4; font-size: 0.8rem; }
      .muted { color: #818181; }
    `,
  ],
})
export class BoardJobDetailComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private board = inject(BoardService);

  job = signal<DispatchedJobDetail | null>(null);

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
    if (id) {
      this.board.getJob(id).subscribe((d) => this.job.set(d));
    }
  }
}
