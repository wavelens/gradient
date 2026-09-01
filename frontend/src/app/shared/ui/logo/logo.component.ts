/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { ThemeService } from '@core/services/theme.service';

/// The wordmark. It is drawn light, so a light background needs the dark file;
/// stated here once rather than at each of the places it appears.
@Component({
  selector: 'gr-logo',
  standalone: true,
  template: '<img [src]="src()" alt="Gradient" />',
  styleUrl: './logo.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class LogoComponent {
  private theme = inject(ThemeService);

  protected src = computed(() =>
    this.theme.resolved() === 'light' ? '/images/logo-black.png' : '/images/logo.svg',
  );
}
