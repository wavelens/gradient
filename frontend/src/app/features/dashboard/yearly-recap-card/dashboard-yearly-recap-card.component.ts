/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { IconComponent } from '@gradient/ui/ui';
import { recapSlideColorSchemeColors } from '@core/yearly-instance-summary/recap-slide-color-scheme-colors';
import { YearlyRecapAutoOpenService } from '@core/yearly-instance-summary/yearly-recap-auto-open.service';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';

const yearlyRecapRefetchIntervalMs = 3_600_000;
const introColors = recapSlideColorSchemeColors.intro;

@Component({
  selector: 'app-dashboard-yearly-recap-card',
  standalone: true,
  imports: [IconComponent],
  template: `
    @if (yearlyRecapAutoOpenService.recap(); as recap) {
      <button type="button" class="banner" (click)="yearlyRecapAutoOpenService.reopenRecapSlideShowFromDashboardCard()"
              [style.--recap-background]="colors.background" [style.--recap-text]="colors.text" [style.--recap-accent]="colors.accent"
              [style.--recap-band-1]="colors.bands[0]" [style.--recap-band-2]="colors.bands[1]" [style.--recap-band-3]="colors.bands[2]">
        <span class="faint-background-logo" aria-hidden="true"></span>
        <span class="text">
          <span class="title">Your Gradient Wrapped {{ recap.year }}</span>
          <span class="subtitle">A recap of one year on this Gradient server.</span>
        </span>
        <span class="bands" aria-hidden="true">
          <span class="band band-1"></span>
          <span class="band band-2"></span>
          <span class="band band-3"></span>
        </span>
        <gr-icon class="play" name="play_circle" size="xl" />
      </button>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './dashboard-yearly-recap-card.component.scss',
})
export class DashboardYearlyRecapCardComponent {
  protected yearlyRecapAutoOpenService = inject(YearlyRecapAutoOpenService);
  protected colors = introColors;

  constructor() {
    runReloadIntervalWhileTabVisible(() => this.yearlyRecapAutoOpenService.refreshCurrentYearlyRecapAndResetAutoOpenStateOnYearChange(), yearlyRecapRefetchIntervalMs);
  }
}
