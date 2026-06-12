/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { BuildStatusCounts } from '@core/models';

type SegKey = 'completed' | 'failed' | 'building' | 'queued';
interface Seg { key: SegKey; count: number; pct: number; label: string; }

const ORDER: { key: SegKey; label: string }[] = [
  { key: 'completed', label: 'completed' },
  { key: 'failed', label: 'failed' },
  { key: 'building', label: 'building' },
  { key: 'queued', label: 'queued' },
];

@Component({
  selector: 'app-segmented-bar',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <span class="segbar" [class.empty]="isEmpty()">
      @if (isEmpty()) {
        <i class="seg seg-empty"></i>
      } @else {
        @for (s of segments(); track s.key) {
          <i class="seg seg-{{ s.key }}" [style.width.%]="s.pct" [title]="s.count + ' ' + s.label"></i>
        }
      }
    </span>
  `,
  styles: [`
    :host { display: inline-block; }
    .segbar { display: flex; height: var(--segbar-h, 9px); width: 100%; border-radius: 5px; overflow: hidden; background: var(--queued, #4b5563); }
    .seg { display: block; height: 100%; transition: filter .12s ease; }
    .seg:hover { filter: brightness(1.25); }
    .seg-empty { width: 100%; background: rgba(255,255,255,.06); }
    .seg-completed { background: var(--success, #22c55e); }
    .seg-failed { background: var(--danger, #ef4444); }
    .seg-building { background: var(--running, #3b82f6); }
    .seg-queued { background: var(--queued, #4b5563); }
  `],
})
export class SegmentedBarComponent {
  counts = input.required<BuildStatusCounts>();

  total = computed(() => {
    const c = this.counts();
    return c.completed + c.failed + c.building + c.queued;
  });

  isEmpty = computed(() => this.total() === 0);

  segments = computed<Seg[]>(() => {
    const c = this.counts();
    const total = this.total();
    if (total === 0) return [];
    return ORDER.map(({ key, label }) => {
      const count = c[key];
      return { key, count, label, pct: (count / total) * 100 };
    }).filter(s => s.count > 0);
  });
}
