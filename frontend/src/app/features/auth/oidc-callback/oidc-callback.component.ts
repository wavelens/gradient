/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, ChangeDetectionStrategy } from '@angular/core';
import { Router } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
import { ApiError } from '@core/services/api.service';
import { links, safeInAppPathOrNull } from '@shared/routing';

@Component({
  selector: 'app-oidc-callback',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.Eager,
  template: '<p>Completing sign-in...</p>',
})
export class OidcCallbackComponent implements OnInit {
  private router = inject(Router);
  private authService = inject(AuthService);

  ngOnInit(): void {
    const next = safeInAppPathOrNull(sessionStorage.getItem('oidc_next'));
    sessionStorage.removeItem('oidc_next');
    this.authService.loginWithCookie().subscribe({
      next: () => void this.router.navigateByUrl(next ?? links.dashboard()),
      error: (error: unknown) => {
        const code = error instanceof ApiError && error.status === 401 ? 'session_cookie_missing' : 'failed';
        void this.router.navigateByUrl(links.loginPageAfterSingleSignOnError(code));
      },
    });
  }
}
