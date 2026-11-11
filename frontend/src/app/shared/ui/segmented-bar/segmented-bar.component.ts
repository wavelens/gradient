/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input, signal } from '@angular/core';
import { BuildStatusCounts } from '@core/models';
import { injectOnScreen } from '@gradient/ui/ui';

export type SegmentTone = 'completed' | 'failed' | 'skipped' | 'aborted' | 'building' | 'queued';
export interface BarSegment { tone: SegmentTone; pct: number; }
interface CountSegment extends BarSegment { label: string; }
interface Tip { text: string; x: number; }

const ORDER: SegmentTone[] = ['completed', 'failed', 'skipped', 'aborted', 'building', 'queued'];

function countSegments(counts: BuildStatusCounts): CountSegment[] {
  const c: Record<SegmentTone, number> = { ...counts, skipped: counts.dependency_failed };
  const total = ORDER.reduce((sum, tone) => sum + c[tone], 0);
  if (total === 0) {
    return counts.substituted > 0 ? [{ tone: 'completed', pct: 100, label: `${counts.substituted} substituted` }] : [];
  }
  return ORDER.map(tone => ({
    tone,
    pct: (c[tone] / total) * 100,
    label: `${c[tone]} ${tone}`,
  }));
}

@Component({
  selector: 'gr-segmented-bar',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <span class="segbar" [class.segbar--build-status-counts]="!segments()" (mouseleave)="tip.set(null)">
      @for (s of shown(); track s.tone) {
        <i class="seg seg-{{ s.tone }}" [class.seg-pulsing]="onScreen() && s.tone === 'building' && s.pct > 0"
           [style.width.%]="s.pct" (mouseenter)="showTip($event, s.label)"></i>
      } @empty {
        <i class="seg seg-empty"></i>
      }
    </span>
    @if (tip(); as t) {
      <span class="tipbox" [style.left.px]="t.x">{{ t.text }}</span>
    }
  `,
  styleUrl: './segmented-bar.component.scss',
})
export class SegmentedBarComponent {
  counts = input<BuildStatusCounts>();
  segments = input<BarSegment[]>();

  tip = signal<Tip | null>(null);
  protected readonly onScreen = injectOnScreen();

  shown = computed<(BarSegment & Partial<CountSegment>)[]>(() => {
    const counts = this.counts();
    return this.segments() ?? (counts ? countSegments(counts) : []);
  });

  showTip(event: Event, text?: string): void {
    if (!text) return;
    const seg = event.target as HTMLElement;
    this.tip.set({ text, x: seg.offsetLeft + seg.offsetWidth / 2 });
  }
}
