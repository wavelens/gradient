/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import type { InputFetch } from '@core/models';
import { inputFetchLabel, inputFetchRatio } from '@shared/evaluation';
import { byteSegments } from '../segmented-bar/byte-segments';
import { type BarSegment, SegmentedBarComponent } from '../segmented-bar/segmented-bar.component';

function segments(row: InputFetch): BarSegment[] {
  switch (row.state) {
    case 'Queued': return [{ tone: 'queued', pct: 100 }];
    case 'Fetching': return byteSegments(row.downloaded_bytes, row.expected_bytes);
    case 'Done': return [{ tone: 'completed', pct: 100 }];
    case 'Failed': return [{ tone: 'failed', pct: 100 }];
  }
}

function percent(row: InputFetch): number | null {
  if (row.state === 'Queued') return 0;
  if (row.state === 'Done') return 100;
  const ratio = row.state === 'Fetching' ? inputFetchRatio(row) : null;
  return ratio === null ? null : Math.round(ratio * 100);
}

@Component({
  selector: 'gr-input-fetch-list',
  standalone: true,
  imports: [SegmentedBarComponent],
  template: `
    <ul class="input-list">
      @for (row of rows(); track $index) {
        <li class="input-row" [attr.data-state]="row.state">
          <span class="input-name">{{ row.name }}</span>
          <gr-segmented-bar class="input-bar" [segments]="row.segments"
                            role="progressbar" aria-valuemin="0" aria-valuemax="100"
                            [attr.aria-label]="row.name" [attr.aria-valuenow]="row.percent"
                            [attr.aria-valuetext]="row.label ? row.state + ', ' + row.label : row.state" />
          <span class="input-size" aria-hidden="true">{{ row.label }}</span>
        </li>
      }
    </ul>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './input-fetch-list.component.scss',
})
export class InputFetchListComponent {
  inputs = input.required<InputFetch[]>();

  protected readonly rows = computed(() => this.inputs().map(row => ({
    name: row.name,
    state: row.state,
    segments: segments(row),
    percent: percent(row),
    label: inputFetchLabel(row),
  })));
}
