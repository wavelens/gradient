/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, Injector, afterNextRender, inject } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { NavigationCancel, NavigationEnd, NavigationError, Router } from '@angular/router';
import { filter } from 'rxjs';

/// Reads that the code can send during router navigation render the page. Reads that the code can send before the first render after the end of navigation render the page too. The router can clear currentNavigation() before the new page can call ngOnInit, so loads inside ngOnInit fall into the second window. The tracker can start in a rendering state. The first request of the app is part of the first page.
@Injectable({ providedIn: 'root' })
export class PageRenderTracker {
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);
  private settling = false;

  constructor() {
    this.settle();
    this.router.events
      .pipe(
        filter((event) => event instanceof NavigationEnd || event instanceof NavigationCancel || event instanceof NavigationError),
        takeUntilDestroyed(),
      )
      .subscribe(() => this.settle());
  }

  isPageStillRendering(): boolean {
    return this.router.currentNavigation() !== null || this.settling;
  }

  private settle(): void {
    this.settling = true;
    afterNextRender(() => (this.settling = false), { injector: this.injector });
  }
}
