/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { IconComponent } from '@gradient/ui/ui';
import type { BuildProgress, BuildProgressPhase } from '@core/models';
import {
  buildProgressBytes, buildProgressPaths, buildProgressRatio, buildProgressTitle,
} from '@shared/evaluation';
import { ratioSegments } from '../segmented-bar/ratio-segments';
import { SegmentedBarComponent } from '../segmented-bar/segmented-bar.component';

const ICONS: Record<BuildProgressPhase, string> = {
  prefetch: 'cloud_download',
  download: 'download',
  upload: 'cloud_upload',
};

@Component({
  selector: 'gr-build-progress',
  standalone: true,
  imports: [IconComponent, SegmentedBarComponent],
  template: `
    @let v = view();
    <div class="build-progress" [class.build-progress--compact]="compact()" [attr.data-phase]="v.phase">
      @if (!compact()) {
        <gr-icon [name]="v.icon" class="bp-icon" size="xl" />
      }
      <span class="bp-title">{{ v.title }}{{ v.percent === null ? '' : ' ' + v.percent + ' %' }}</span>
      <gr-segmented-bar class="bp-bar" [segments]="v.segments"
                        role="progressbar" aria-valuemin="0" aria-valuemax="100"
                        [attr.aria-label]="v.title" [attr.aria-valuenow]="v.percent" />
      <span class="bp-amounts">
        <span class="bp-bytes">{{ v.bytes }}</span>
        @if (v.paths) {
          <span class="bp-paths">{{ v.paths }}</span>
        }
      </span>
    </div>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
  styleUrl: './build-progress.component.scss',
})
export class BuildProgressComponent {
  progress = input.required<BuildProgress>();
  compact = input(false);

  protected readonly view = computed(() => {
    const p = this.progress();
    const ratio = buildProgressRatio(p);
    return {
      phase: p.phase,
      icon: ICONS[p.phase],
      title: buildProgressTitle(p),
      percent: ratio === null ? null : Math.floor(ratio * 100),
      segments: ratio === null ? ratioSegments(0, null) : ratioSegments(ratio, 1),
      bytes: buildProgressBytes(p),
      paths: buildProgressPaths(p),
    };
  });
}
