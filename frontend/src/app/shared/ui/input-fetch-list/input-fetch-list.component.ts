/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, input } from '@angular/core';
import { IconComponent } from '@gradient/ui/ui';
import type { InputFetch } from '@core/models';
import { inputFetchLabel, inputFetchRatio } from '@shared/evaluation';

@Component({
  selector: 'gr-input-fetch-list',
  standalone: true,
  imports: [IconComponent],
  template: `
    <ul class="input-list">
      @for (row of inputs(); track row.name) {
        <li class="input-row" [attr.data-state]="row.state">
          @switch (row.state) {
            @case ('Queued') { <gr-icon name="schedule" size="sm" class="input-icon" /> }
            @case ('Fetching') { <gr-icon name="progress_activity" size="sm" class="input-icon gr-spin" /> }
            @case ('Done') { <gr-icon name="check_circle" size="sm" class="input-icon" /> }
            @case ('Failed') { <gr-icon name="error" size="sm" class="input-icon" /> }
          }
          <span class="input-name">{{ row.name }}</span>
          @let r = ratio(row);
          @if (r !== null) {
            <div class="input-bar" role="progressbar" [attr.aria-label]="row.name"
                 [attr.aria-valuenow]="round(r * 100)" aria-valuemin="0" aria-valuemax="100">
              <div class="input-bar-fill" [style.width.%]="r * 100"></div>
            </div>
          }
          @if (row.downloaded_bytes > 0) {
            <span class="input-size">{{ label(row) }}</span>
          }
        </li>
      }
    </ul>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './input-fetch-list.component.scss',
})
export class InputFetchListComponent {
  inputs = input.required<InputFetch[]>();
  protected readonly ratio = inputFetchRatio;
  protected readonly label = inputFetchLabel;
  protected readonly round = Math.round;
}
