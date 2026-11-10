/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, DestroyRef, ElementRef, Injector, afterNextRender, inject, computed, viewChild, ChangeDetectionStrategy } from '@angular/core';
import { Router, RouterOutlet, NavigationCancel, NavigationEnd, NavigationError, NavigationSkipped, NavigationStart } from '@angular/router';
import { takeUntilDestroyed, toSignal } from '@angular/core/rxjs-interop';
import { of, timer } from 'rxjs';
import { filter, map, skip, switchMap } from 'rxjs/operators';
import { FooterComponent, FooterLink } from '@gradient/ui/chrome';
import { EmptyStateComponent, LoadingSpinnerComponent, ToastComponent } from '@gradient/ui/ui';
import { AppHeaderComponent } from '@shared/chrome/header/header.component';
import { CommandPaletteComponent } from '@shared/chrome/command-palette/command-palette.component';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ThemeService } from '@core/services/theme.service';
import { followOverscroll } from '@core/overscroll/overscroll';
import { WhatsNewDialogComponent } from '@core/whats-new/whats-new-dialog.component';
import { ConfirmDialogComponent } from '@shared/confirm/confirm-dialog.component';

const navigationLoadingDelayMs = 300;

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
  imports: [RouterOutlet, AppHeaderComponent, FooterComponent, CommandPaletteComponent, WhatsNewDialogComponent, ConfirmDialogComponent, ToastComponent, LoadingSpinnerComponent, EmptyStateComponent],
  templateUrl: './app.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './app.scss'
})
export class App {
  authService = inject(AuthService);
  private router = inject(Router);
  private theme = inject(ThemeService);
  protected readonly config = inject(ConfigService);

  protected readonly footerLinks = FOOTER_LINKS;
  private readonly main = viewChild<ElementRef<HTMLElement>>('main');
  protected get version(): string | null {
    const { backendVersion, backendCommit } = this.config;
    if (!backendVersion) return null;
    return backendCommit ? `v${backendVersion} · ${backendCommit.slice(0, 8)}` : `v${backendVersion}`;
  }

  constructor() {
    inject(DestroyRef).onDestroy(followOverscroll(window));
    const injector = inject(Injector);
    this.router.events
      .pipe(filter((e) => e instanceof NavigationEnd), skip(1), takeUntilDestroyed())
      .subscribe(() => afterNextRender(() => this.moveFocusToMainAfterRouteChange(), { injector }));
  }

  private moveFocusToMainAfterRouteChange(): void {
    const main = this.main()?.nativeElement;
    if (!main) return;
    const focused = document.activeElement;
    if (focused && focused !== document.body && main.contains(focused)) return;
    main.focus({ preventScroll: true });
  }

  protected skipToMain(event: Event): void {
    event.preventDefault();
    this.main()?.nativeElement.focus();
  }

  protected reloadPage(): void {
    globalThis.location.reload();
  }

  protected readonly isRouteNavigationTakingLong = toSignal(
    this.router.events.pipe(
      filter((e) => e instanceof NavigationStart || e instanceof NavigationEnd || e instanceof NavigationCancel
        || e instanceof NavigationError || e instanceof NavigationSkipped),
      switchMap((e) => (e instanceof NavigationStart ? timer(navigationLoadingDelayMs).pipe(map(() => true)) : of(false))),
    ),
    { initialValue: false },
  );

  protected readonly routeData = toSignal(
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
