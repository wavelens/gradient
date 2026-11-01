/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { FormBuilder, FormGroup, ReactiveFormsModule, Validators } from '@angular/forms';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { take } from 'rxjs';
import { environment } from '@environments/environment';
import {
  ButtonComponent,
  CheckboxComponent,
  FormFieldComponent,
  InputDirective,
  LogoComponent,
  MessageBannerComponent,
  PasswordInputComponent,
} from '@gradient/ui/ui';

const LOGIN_ERRORS: Record<string, string> = {
  deactivated: 'This account is deactivated.',
  account_exists: 'An account already exists with this username or email.',
  username_taken: 'The username is taken. Sign in and connect the account from your profile instead.',
  no_verified_email: 'The account has no verified primary email.',
  unusable_username: 'The username of the account cannot be used here.',
  failed: 'Sign-in failed. Please try again.',
};

@Component({
  selector: 'app-login',
  standalone: true,
  imports: [CommonModule, ReactiveFormsModule, RouterModule, ButtonComponent, FormFieldComponent, InputDirective, MessageBannerComponent, PasswordInputComponent, CheckboxComponent, LogoComponent],
  templateUrl: './login.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './login.component.scss',
})
export class LoginComponent {
  private fb = inject(FormBuilder);
  private authService = inject(AuthService);
  private router = inject(Router);
  private route = inject(ActivatedRoute);
  private config = inject(ConfigService);

  loginForm: FormGroup;
  errorMessage = signal<string | null>(null);
  loading = signal(false);
  showPassword = signal(false);
  get oidcEnabled() { return this.config.oidcEnabled; }
  get logoUrl() { return this.config.logoUrl; }
  get loginProviders() { return this.config.loginProviders; }
  get passwordLoginEnabled() { return this.config.passwordLoginEnabled; }
  get registrationDisabled() { return this.config.registrationDisabled; }

  constructor() {
    this.loginForm = this.fb.group({
      username: ['', [Validators.required]],
      password: ['', [Validators.required]],
      rememberMe: [false],
    });

    const loginError = this.route.snapshot.queryParamMap.get('login_error');
    if (loginError) {
      this.errorMessage.set(LOGIN_ERRORS[loginError] ?? LOGIN_ERRORS['failed']);
    }

    this.authService.initialized$.pipe(take(1)).subscribe(() => {
      if (this.authService.isAuthenticated()) {
        this.navigateAfterLogin();
      }
    });
  }

  private navigateAfterLogin(): void {
    const next = this.route.snapshot.queryParamMap.get('next');
    if (next && next.startsWith('/')) {
      this.router.navigateByUrl(next);
    } else {
      this.router.navigate(['/']);
    }
  }

  onSubmit(): void {
    if (this.loginForm.valid) {
      this.loading.set(true);
      this.errorMessage.set(null);

      const { username, password, rememberMe } = this.loginForm.value;

      this.authService.login(username, password, rememberMe).subscribe({
        next: () => {
          this.loading.set(false);
          this.navigateAfterLogin();
        },
        error: (error) => {
          this.loading.set(false);
          this.errorMessage.set(error.message || 'Login failed. Please try again.');
        },
      });
    }
  }

  loginWithOIDC(): void {
    this.redirectTo('auth/oidc/login');
  }

  loginWithProvider(id: string): void {
    this.redirectTo(`auth/providers/${encodeURIComponent(id)}/login`);
  }

  private redirectTo(path: string): void {
    const next = this.route.snapshot.queryParamMap.get('next');
    if (next && next.startsWith('/')) {
      sessionStorage.setItem('oidc_next', next);
    }
    window.location.href = `${environment.apiUrl}/${path}`;
  }
}
