/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input, signal } from '@angular/core';
import { BuildStatusCounts } from '@core/models';

type SegKey = 'completed' | 'failed' | 'building' | 'queued';
interface Seg { key: SegKey; count: number; pct: number; label: string; }
interface Tip { text: string; x: number; }

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
    <span class="segbar" [class.empty]="isEmpty()" (mouseleave)="tip.set(null)">
      @if (allSubstituted()) {
        <i class="seg seg-completed" style="width: 100%"
           (mouseenter)="showTip($event, counts().substituted + ' substituted')"></i>
      } @else if (isEmpty()) {
        <i class="seg seg-empty"></i>
      } @else {
        @for (s of segments(); track s.key) {
          <i class="seg seg-{{ s.key }}" [style.width.%]="s.pct"
             (mouseenter)="showTip($event, s.count + ' ' + s.label)"></i>
        }
      }
    </span>
    @if (tip(); as t) {
      <span class="tipbox" [style.left.px]="t.x">{{ t.text }}</span>
    }
  `,
  styleUrl: './segmented-bar.component.scss',
})
export class SegmentedBarComponent {
  counts = input.required<BuildStatusCounts>();

  tip = signal<Tip | null>(null);

  total = computed(() => {
    const c = this.counts();
    return c.completed + c.failed + c.building + c.queued;
  });

  allSubstituted = computed(() => this.total() === 0 && this.counts().substituted > 0);

  isEmpty = computed(() => this.total() === 0 && !this.allSubstituted());

  segments = computed<Seg[]>(() => {
    const c = this.counts();
    const total = this.total();
    if (total === 0) return [];
    return ORDER.map(({ key, label }) => {
      const count = c[key];
      return { key, count, label, pct: (count / total) * 100 };
    });
  });

  showTip(event: Event, text: string): void {
    const seg = event.target as HTMLElement;
    this.tip.set({ text, x: seg.offsetLeft + seg.offsetWidth / 2 });
  }
}
