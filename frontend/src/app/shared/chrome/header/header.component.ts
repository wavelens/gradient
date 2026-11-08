/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, effect, inject, signal, untracked, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import { BadgeComponent, IconComponent } from '@gradient/ui/ui';
import { Brand, HeaderComponent, NavLink } from '@gradient/ui/chrome';
import { AuthService } from '@core/services/auth.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { CommandPaletteService } from '../command-palette/command-palette.service';
import { ConfigService } from '@core/services/config.service';
import { UserMenuComponent } from './user-menu.component';
import { links } from '@shared/routing';

const BRAND: Brand = { label: 'Gradient', href: String(links.dashboard()) };
const NAV: (NavLink & { signedIn?: true; publicList?: true })[] = [
  { label: 'Dashboard', href: String(links.dashboard()), exact: true, signedIn: true },
  { label: 'Projects', href: String(links.projects()), publicList: true },
  { label: 'Caches', href: String(links.caches()), publicList: true },
  { label: 'Teams', href: String(links.teams()), signedIn: true },
  { label: 'Job Board', href: '/board', signedIn: true },
];

@Component({
  selector: 'app-header',
  standalone: true,
  imports: [HeaderComponent, BadgeComponent, IconComponent, UserMenuComponent, RouterLink],
  templateUrl: './header.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './header.component.scss',
})
export class AppHeaderComponent {
  protected readonly links = links;
  authService = inject(AuthService);
  protected palette = inject(CommandPaletteService);
  protected router = inject(Router);
  private config = inject(ConfigService);
  protected invites = inject(PendingInvitesService);

  protected readonly brand: Brand = { ...BRAND, logoUrl: this.config.logoUrl };
  protected nav = computed(() => {
    const signedIn = this.authService.isAuthenticated();
    const listed = this.config.publicListing;
    return NAV.filter((link) => signedIn || (!link.signedIn && (listed || !link.publicList)));
  });

  protected userMenu = viewChild<UserMenuComponent>('userMenu');
  private pendingMenuOrigin = signal<HTMLElement | null>(null);

  get registrationDisabled() { return this.config.registrationDisabled; }

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
