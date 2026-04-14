/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormBuilder, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { Router, RouterModule } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { take } from 'rxjs';
import { environment } from '@environments/environment';

@Component({
  selector: 'app-login',
  standalone: true,
  imports: [CommonModule, ReactiveFormsModule, RouterModule],
  templateUrl: './login.component.html',
  styleUrl: './login.component.scss',
})
export class LoginComponent {
  private fb = inject(FormBuilder);
  private authService = inject(AuthService);
  private router = inject(Router);
  private config = inject(ConfigService);

  loginForm: FormGroup;
  errorMessage = signal<string | null>(null);
  loading = signal(false);
  showPassword = signal(false);
  get oidcEnabled() { return this.config.oidcEnabled; }
  get oidcRequired() { return this.config.oidcRequired; }
  get registrationDisabled() { return this.config.registrationDisabled; }

  constructor() {
    this.loginForm = this.fb.group({
      username: ['', [Validators.required]],
      password: ['', [Validators.required]],
      rememberMe: [false],
    });

    this.authService.initialized$.pipe(take(1)).subscribe(() => {
      if (this.authService.isAuthenticated()) {
        this.router.navigate(['/']);
      }
    });
  }

  onSubmit(): void {
    if (this.loginForm.valid) {
      this.loading.set(true);
      this.errorMessage.set(null);

      const { username, password, rememberMe } = this.loginForm.value;

      this.authService.login(username, password, rememberMe).subscribe({
        next: () => {
          this.loading.set(false);
          this.router.navigate(['/']);
        },
        error: (error) => {
          this.loading.set(false);
          this.errorMessage.set(error.message || 'Login failed. Please try again.');
        },
      });
    }
  }

  loginWithOIDC(): void {
    window.location.href = `${environment.apiUrl}/auth/oidc/login`;
  }
}
