/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, ChangeDetectionStrategy } from '@angular/core';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { ThemeService, ThemePreference } from '@core/services/theme.service';

export interface SectionLink {
  path: string;
  label: string;
}

export const STYLEGUIDE_NAV: SectionLink[] = [
  { path: '', label: 'Overview' },
  { path: 'foundations', label: 'Foundations' },
  { path: 'elements', label: 'Elements' },
  { path: 'components', label: 'Components' },
  { path: 'patterns', label: 'Patterns' },
];

@Component({
  selector: 'app-styleguide-shell',
  standalone: true,
  imports: [RouterLink, RouterLinkActive, RouterOutlet],
  templateUrl: './styleguide-shell.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './styleguide-shell.component.scss',
})
export class StyleguideShellComponent {
  private themeService = inject(ThemeService);

  nav = STYLEGUIDE_NAV;
  theme = this.themeService.preference;
  themes: ThemePreference[] = ['system', 'light', 'dark'];

  setTheme(pref: ThemePreference): void {
    this.themeService.set(pref);
  }
}
