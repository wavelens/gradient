/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { NavigationEnd, Router, RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { filter, map } from 'rxjs';
import { Crumb, PageLayoutComponent } from '@shared/ui';
import { ThemeService, ThemePreference } from '@core/services/theme.service';

export interface SectionLink {
  path: string;
  label: string;
  title: string;
}

export const STYLEGUIDE_NAV: SectionLink[] = [
  { path: '', label: 'Overview', title: 'Gradient Design System' },
  { path: 'foundations', label: 'Foundations', title: 'Foundations' },
  { path: 'elements', label: 'Elements', title: 'Elements' },
  { path: 'components', label: 'Components', title: 'Components' },
  { path: 'patterns', label: 'Patterns', title: 'Patterns' },
];

/// The guide is built out of the same shell it documents, so a page archetype that
/// breaks here breaks visibly.
@Component({
  selector: 'app-styleguide-shell',
  standalone: true,
  imports: [RouterLink, RouterLinkActive, RouterOutlet, PageLayoutComponent],
  templateUrl: './styleguide-shell.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './styleguide-shell.component.scss',
})
export class StyleguideShellComponent {
  private themeService = inject(ThemeService);
  private router = inject(Router);

  private url = toSignal(
    this.router.events.pipe(
      filter((e) => e instanceof NavigationEnd),
      map(() => this.router.url),
    ),
    { initialValue: this.router.url },
  );

  nav = STYLEGUIDE_NAV;
  theme = this.themeService.preference;
  themes: ThemePreference[] = ['system', 'light', 'dark'];

  section = computed(() => {
    const path = this.url().replace(/^\/styleguide\/?/, '').split(/[?#]/)[0];
    return STYLEGUIDE_NAV.find((s) => s.path === path) ?? STYLEGUIDE_NAV[0];
  });

  crumbs = computed<Crumb[]>(() =>
    this.section().path
      ? [{ label: 'Style Guide', link: ['/styleguide'] }, { label: this.section().label }]
      : [{ label: 'Style Guide' }],
  );

  setTheme(pref: ThemePreference): void {
    this.themeService.set(pref);
  }
}
