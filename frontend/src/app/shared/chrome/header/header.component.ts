/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { Router, RouterLink } from '@angular/router';
import { IconComponent } from '@gradient/ui/ui';
import { Brand, HeaderComponent, NavLink } from '@gradient/ui/chrome';
import { AuthService } from '@core/services/auth.service';
import { CommandPaletteService } from '../command-palette/command-palette.service';
import { ConfigService } from '@core/services/config.service';

const BRAND: Brand = { label: 'Gradient', href: '/' };
const SIGNED_IN_NAV: NavLink[] = [
  { label: 'Dashboard', href: '/', exact: true },
  { label: 'Job Board', href: '/board' },
];
const PUBLIC_NAV: NavLink[] = [
  { label: 'Projects', href: '/projects' },
  { label: 'Caches', href: '/caches' },
];

@Component({
  selector: 'app-header',
  standalone: true,
  imports: [HeaderComponent, IconComponent, RouterLink],
  templateUrl: './header.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './header.component.scss',
})
export class AppHeaderComponent {
  authService = inject(AuthService);
  protected palette = inject(CommandPaletteService);
  protected router = inject(Router);
  private config = inject(ConfigService);

  protected readonly brand = BRAND;
  protected nav = computed(() =>
    this.authService.isAuthenticated() ? [...SIGNED_IN_NAV, ...PUBLIC_NAV] : PUBLIC_NAV,
  );

  get registrationDisabled() { return this.config.registrationDisabled; }

  logout(): void {
    this.authService.logout().subscribe();
  }
}
