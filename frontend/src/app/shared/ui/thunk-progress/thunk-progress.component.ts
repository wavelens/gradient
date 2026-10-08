/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import type { ThunkProgress } from '@shared/evaluation';
import { SegmentedBarComponent } from '../segmented-bar/segmented-bar.component';

@Component({
  selector: 'gr-thunk-progress',
  standalone: true,
  imports: [SegmentedBarComponent],
  template: `
    <gr-segmented-bar class="thunk-bar" [segments]="progress().segments"
                      role="progressbar" aria-valuemin="0" aria-valuemax="100"
                      aria-label="Thunks evaluated" [attr.aria-valuenow]="progress().percent"
                      [attr.aria-valuetext]="progress().label" />
    <span class="thunk-label" aria-hidden="true">{{ progress().label }}</span>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './thunk-progress.component.scss',
})
export class ThunkProgressComponent {
  progress = input.required<ThunkProgress>();
}
