/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, inject, input, signal, ChangeDetectionStrategy } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { NavigationEnd, Router, RouterLink, isActive } from '@angular/router';
import { filter } from 'rxjs';
import { activeLinkMatch } from '../active-link-match';
import { DrawerDirective } from '../../ui/drawer/drawer.directive';
import { IconComponent } from '../../ui/icon/icon.component';
import { LogoComponent } from '../../ui/logo/logo.component';
import { Brand, NavLink } from '../types';

@Component({
  selector: 'gr-header',
  standalone: true,
  imports: [RouterLink, DrawerDirective, IconComponent, LogoComponent],
  templateUrl: './header.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './header.component.scss',
})
export class HeaderComponent {
  brand = input.required<Brand>();
  nav = input<NavLink[]>([]);

  private readonly router = inject(Router);
  protected readonly menuOpen = signal(false);
  protected readonly navigationLinksWithCurrentPageSignals = computed(() =>
    this.nav().map((link) => {
      const matches = [
        isActive(link.href, this.router, activeLinkMatch(link.exact ?? false)),
        ...(link.activePathPrefixes ?? []).map((prefix) => isActive(prefix, this.router, activeLinkMatch(false))),
      ];
      return { link, current: computed(() => matches.some((match) => match())) };
    }),
  );

  constructor() {
    this.router.events
      .pipe(
        filter((event) => event instanceof NavigationEnd),
        takeUntilDestroyed(),
      )
      .subscribe(() => this.closeMenu());
  }

  protected toggleMenu(): void {
    this.menuOpen.update((open) => !open);
  }

  protected closeMenu(): void {
    this.menuOpen.set(false);
  }
}
