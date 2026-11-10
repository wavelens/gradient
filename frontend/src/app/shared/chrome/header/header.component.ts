/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, effect, inject, signal, untracked, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import { IconComponent } from '@gradient/ui/ui';
import { Brand, HeaderComponent, NavLink } from '@gradient/ui/chrome';
import { AuthService } from '@core/services/auth.service';
import { CommandPaletteService } from '../command-palette/command-palette.service';
import { ConfigService } from '@core/services/config.service';
import { UserMenuComponent } from './user-menu.component';
import { getInAppReturnPathIfNotErrorPage, links } from '@shared/routing';

const BRAND: Brand = { label: 'Gradient', href: String(links.dashboard()) };
const NAV: (NavLink & { signedIn?: true; publicList?: true; superuser?: true })[] = [
  { label: 'Dashboard', href: String(links.dashboard()), exact: true, signedIn: true },
  { label: 'Projects', href: String(links.projects()), activePathPrefixes: ['/project'], publicList: true },
  { label: 'Caches', href: String(links.caches()), publicList: true },
  { label: 'Teams', href: String(links.teams()), activePathPrefixes: ['/team'], signedIn: true },
  { label: 'Job Board', href: '/board', signedIn: true },
  { label: 'Admin', href: '/admin', activePathPrefixes: ['/admin'], signedIn: true, superuser: true },
];

@Component({
  selector: 'app-header',
  standalone: true,
  imports: [HeaderComponent, IconComponent, UserMenuComponent, RouterLink],
  templateUrl: './header.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './header.component.scss',
})
export class AppHeaderComponent {
  protected readonly links = links;
  authService = inject(AuthService);
  protected palette = inject(CommandPaletteService);
  private router = inject(Router);
  private config = inject(ConfigService);

  protected readonly brand: Brand = { ...BRAND, logoUrl: this.config.logoUrl };
  protected nav = computed(() => {
    const signedIn = this.authService.isAuthenticated();
    const listed = this.config.publicListing;
    const superuser = this.authService.user()?.superuser === true;
    return NAV.filter((link) => (superuser || !link.superuser) && (signedIn || (!link.signedIn && (listed || !link.publicList))));
  });

  protected userMenu = viewChild<UserMenuComponent>('userMenu');
  private pendingMenuOrigin = signal<HTMLElement | null>(null);

  get registrationDisabled() { return this.config.registrationDisabled; }
  protected readonly urlToReturnToAfterSignIn = computed(() => {
    const finalUrl = this.router.lastSuccessfulNavigation()?.finalUrl;
    const current = finalUrl ? this.router.serializeUrl(finalUrl) : this.router.url;
    if (!current.startsWith('/error/')) return current;
    return getInAppReturnPathIfNotErrorPage(finalUrl?.queryParamMap.get('from')) ?? '/';
  });
  protected readonly isAccountRoute = computed(() => this.urlToReturnToAfterSignIn().startsWith('/account/'));

  constructor() {
    effect(() => {
      const menu = this.userMenu();
      const origin = this.pendingMenuOrigin();
      if (!menu || !origin) return;
      untracked(() => {
        this.pendingMenuOrigin.set(null);
        menu.toggle(origin);
      });
    });
  }

  protected toggleUserMenu(trigger: HTMLElement): void {
    const menu = this.userMenu();
    if (menu) menu.toggle(trigger);
    else this.pendingMenuOrigin.set(trigger);
  }
}
