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
const NAV: (NavLink & { signedIn?: true })[] = [
  { label: 'Dashboard', href: '/', exact: true, signedIn: true },
  { label: 'Projects', href: '/projects' },
  { label: 'Caches', href: '/caches' },
  { label: 'Teams', href: '/teams', signedIn: true },
  { label: 'Job Board', href: '/board', signedIn: true },
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
  protected nav = computed(() => {
    const signedIn = this.authService.isAuthenticated();
    return NAV.filter((link) => signedIn || !link.signedIn);
  });

  get registrationDisabled() { return this.config.registrationDisabled; }

  logout(): void {
    this.authService.logout().subscribe();
  }
}
