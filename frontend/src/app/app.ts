/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, DestroyRef, inject, computed, ChangeDetectionStrategy } from '@angular/core';
import { Router, RouterOutlet, NavigationEnd } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { filter, map } from 'rxjs/operators';
import { FooterComponent, FooterLink } from '@gradient/ui/chrome';
import { AppHeaderComponent } from '@shared/chrome/header/header.component';
import { CommandPaletteComponent } from '@shared/chrome/command-palette/command-palette.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ThemeService } from '@core/services/theme.service';
import { followOverscroll } from '@core/overscroll/overscroll';

const FOOTER_LINKS: FooterLink[] = [
  {
    prefix: 'Licensed under',
    label: 'AGPL-3.0-only',
    href: 'https://github.com/wavelens/gradient/blob/main/LICENSE',
  },
  { label: 'GitHub', href: 'https://github.com/wavelens/gradient', icon: 'code' },
  { label: 'Wavelens', href: 'https://wavelens.io' },
];

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, AppHeaderComponent, FooterComponent, CommandPaletteComponent],
  templateUrl: './app.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './app.scss'
})
export class App {
  authService = inject(AuthService);
  private router = inject(Router);
  private theme = inject(ThemeService);
  private config = inject(ConfigService);

  protected readonly footerLinks = FOOTER_LINKS;
  protected get version(): string | null {
    return this.config.backendVersion ? `v${this.config.backendVersion}` : null;
  }

  constructor() {
    inject(DestroyRef).onDestroy(followOverscroll(window));
  }

  private routeData = toSignal(
    this.router.events.pipe(
      filter((e) => e instanceof NavigationEnd),
      map(() => {
        let route = this.router.routerState.root;
        while (route.firstChild) route = route.firstChild;
        return route.snapshot.data;
      })
    ),
    { initialValue: null }
  );

  showFooter = computed(() => {
    const data = this.routeData();
    return data !== null && !data['hideFooter'];
  });
}
