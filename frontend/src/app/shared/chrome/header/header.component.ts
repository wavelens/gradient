/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import { IconComponent, MenuComponent, MenuItem } from '@gradient/ui/ui';
import { Brand, HeaderComponent, NavLink } from '@gradient/ui/chrome';
import { AuthService } from '@core/services/auth.service';
import { CommandPaletteService } from '../command-palette/command-palette.service';
import { ConfigService } from '@core/services/config.service';
import { WhatsNewService } from '@core/whats-new/whats-new.service';
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
  imports: [HeaderComponent, IconComponent, MenuComponent, RouterLink],
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
  private whatsNew = inject(WhatsNewService);

  protected readonly brand = BRAND;
  protected nav = computed(() => {
    const signedIn = this.authService.isAuthenticated();
    const listed = this.config.publicListing;
    return NAV.filter((link) => signedIn || (!link.signedIn && (listed || !link.publicList)));
  });

  protected readonly userMenuItems: MenuItem[] = [
    { label: 'Settings', icon: 'settings', routerLink: links.settings('profile') },
    { label: 'My Invites', icon: 'mail', routerLink: links.settings('invites') },
    ...(this.whatsNew.hasNotes ? [{ label: "What's New", icon: 'new_releases', command: () => this.whatsNew.showAll() }] : []),
    { separator: true },
    { label: 'Log out', icon: 'logout', command: () => this.logout() },
  ];

  get registrationDisabled() { return this.config.registrationDisabled; }

  logout(): void {
    this.authService.logout().subscribe();
  }
}
