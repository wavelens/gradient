/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject } from '@angular/core';
import { Router } from '@angular/router';
import { AuthService } from '@core/services/auth.service';

@Component({
  selector: 'app-oidc-callback',
  standalone: true,
  template: '<p>Completing sign in...</p>',
})
export class OidcCallbackComponent implements OnInit {
  private router = inject(Router);
  private authService = inject(AuthService);

  ngOnInit(): void {
    this.authService.loginWithCookie().subscribe({
      next: () => this.router.navigate(['/']),
      error: () => this.router.navigate(['/account/login']),
    });
  }
}
