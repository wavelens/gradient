/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import {
  FormBuilder,
  FormGroup,
  ReactiveFormsModule,
  Validators,
  AbstractControl,
  ValidationErrors,
} from '@angular/forms';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { environment } from '@environments/environment';
import { ApiError, apiErrorMessage } from '@core/services/api.service';
import { apiErrorCodeToFormFieldName, checkUsernameAvailability } from '@core/services/account-form-api-checks';
import { Observable } from 'rxjs';
import {
  ButtonComponent,
  FormFieldComponent,
  IconComponent,
  InputDirective,
  LogoComponent,
  MessageBannerComponent,
  PasswordInputComponent,
} from '@gradient/ui/ui';
import { links, readInAppNextPathFromRouteSnapshot } from '@shared/routing';
import { injectRevealFirstInvalid } from '@shared/forms';
import { USERNAME_PATTERN, USERNAME_RULE } from '../username-rule';

const PASSWORD_SPECIAL_CHARACTERS = '!@#$%^&*()_+-=[]{}|;:,.<>?';

@Component({
  selector: 'app-register',
  standalone: true,
  imports: [CommonModule, ReactiveFormsModule, RouterModule, ButtonComponent, IconComponent, FormFieldComponent, InputDirective, MessageBannerComponent, PasswordInputComponent, LogoComponent],
  templateUrl: './register.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './register.component.scss',
})
export class RegisterComponent {
  protected readonly links = links;
  private fb = inject(FormBuilder);
  private authService = inject(AuthService);
  private router = inject(Router);
  private route = inject(ActivatedRoute);
  private config = inject(ConfigService);
  private host = inject(ElementRef<HTMLElement>);
  private revealFirstInvalid = injectRevealFirstInvalid();
  protected readonly specialCharacters = PASSWORD_SPECIAL_CHARACTERS;
  protected readonly usernameRule = USERNAME_RULE;
  protected readonly nextPagePathAfterSignIn = readInAppNextPathFromRouteSnapshot(this.route.snapshot);

  registerForm: FormGroup;
  errorMessage = signal<string | null>(null);
  successMessage = signal<string | null>(null);
  loading = signal(false);
  showPassword = signal(false);
  showConfirmPassword = signal(false);

  constructor() {
    this.registerForm = this.fb.group(
      {
        username: [
          '',
          [Validators.required, Validators.pattern(USERNAME_PATTERN)],
          [this.usernameValidator.bind(this)],
        ],
        name: ['', [Validators.required]],
        email: ['', [Validators.required, Validators.email]],
        password: [
          '',
          [
            Validators.required,
            Validators.minLength(8),
            Validators.maxLength(128),
            this.passwordValidator,
          ],
        ],
        confirmPassword: ['', [Validators.required]],
      },
      { validators: this.passwordMatchValidator }
    );
  }

  // Custom validator for password requirements
  private passwordValidator(control: AbstractControl): ValidationErrors | null {
    const value = control.value;
    if (!value) return null;

    const hasUpperCase = /\p{Lu}/u.test(value);
    const hasLowerCase = /\p{Ll}/u.test(value);
    const hasNumeric = /[0-9]/.test(value);
    const hasSpecialChar = [...value].some((c) => PASSWORD_SPECIAL_CHARACTERS.includes(c));
    const codes = [...value].map((c) => c.codePointAt(0)! & 0xff);
    const hasFourConsecutiveAscendingCharacters = codes.some((code, i) =>
      i + 3 < codes.length && codes[i + 1] === code + 1 && codes[i + 2] === code + 2 && codes[i + 3] === code + 3);
    const characters = [...value];
    const passwordHasNoCharacterRepeatedThreeTimesInARow = !characters.some((c, i) =>
      i + 2 < characters.length && characters[i + 1] === c && characters[i + 2] === c);
    const passwordDoesNotContainPasswordWord = !value.toLowerCase().includes('password');

    const valid = hasUpperCase && hasLowerCase && hasNumeric && hasSpecialChar
      && !hasFourConsecutiveAscendingCharacters
      && passwordHasNoCharacterRepeatedThreeTimesInARow
      && passwordDoesNotContainPasswordWord;

    if (!valid) {
      return {
        passwordStrength: {
          hasUpperCase,
          hasLowerCase,
          hasNumeric,
          hasSpecialChar,
          hasFourConsecutiveAscendingCharacters,
          passwordHasNoCharacterRepeatedThreeTimesInARow,
          passwordDoesNotContainPasswordWord,
        },
      };
    }

    return null;
  }

  // Custom validator to check if passwords match
  private passwordMatchValidator(control: AbstractControl): ValidationErrors | null {
    const password = control.get('password');
    const confirmPassword = control.get('confirmPassword');

    if (!password || !confirmPassword) return null;

    return password.value === confirmPassword.value
      ? null
      : { passwordMismatch: true };
  }

  private usernameValidator(control: AbstractControl): Observable<ValidationErrors | null> {
    return checkUsernameAvailability(this.authService, control.value);
  }

  get usernameInvalid(): boolean {
    const control = this.username;
    if (!control) return false;
    return (control.touched && control.invalid) || control.hasError('usernameRefused') || control.hasError('usernameCheckFailed');
  }

  get usernameRefusal(): string {
    return this.username?.getError('usernameRefused') ?? '';
  }

  get emailRefusal(): string {
    return this.email?.getError('emailRefused') ?? '';
  }

  recheckUsername(): void {
    this.username?.updateValueAndValidity();
  }

  onSubmit(): void {
    if (this.loading() || this.registerForm.pending) return;
    if (this.registerForm.invalid) {
      this.registerForm.markAllAsTouched();
      this.revealFirstInvalid(() => this.host.nativeElement);
      return;
    }
    this.loading.set(true);
    this.errorMessage.set(null);
    this.successMessage.set(null);

    const { username, name, email, password } = this.registerForm.value;

    this.authService.register({ username, name, email, password }).subscribe({
      next: () => {
        this.loading.set(false);
        if (this.config.emailVerificationEnabled) {
          this.successMessage.set(
            'Registration successful! Please check your email to verify your account before signing in.'
          );
        } else {
          this.router.navigateByUrl(links.loginAfterRegister(username, this.nextPagePathAfterSignIn));
        }
      },
      error: (error: unknown) => {
        this.loading.set(false);
        const field = error instanceof ApiError && error.code ? apiErrorCodeToFormFieldName[error.code] : undefined;
        if (field && error instanceof ApiError) {
          const control = this.registerForm.get(field);
          control?.setErrors({ [`${field}Refused`]: error.message });
          control?.markAsTouched();
          this.revealFirstInvalid(() => this.host.nativeElement);
          return;
        }
        this.errorMessage.set(apiErrorMessage(error, 'Registration failed. Please try again.'));
      },
    });
  }

  get username() {
    return this.registerForm.get('username');
  }

  get name() {
    return this.registerForm.get('name');
  }

  get email() {
    return this.registerForm.get('email');
  }

  get password() {
    return this.registerForm.get('password');
  }

  get confirmPassword() {
    return this.registerForm.get('confirmPassword');
  }

  /// The mismatch error lives on the group, so the confirm field has to ask for it.
  get confirmInvalid(): boolean {
    const c = this.confirmPassword;
    if (!c?.touched) return false;
    return !!c.invalid || !!this.registerForm.errors?.['passwordMismatch'];
  }

  get confirmError(): string {
    if (this.confirmPassword?.errors?.['required']) return 'Please confirm your password';
    return this.registerForm.errors?.['passwordMismatch'] ? 'Passwords do not match' : '';
  }

  get oidcEnabled() { return this.config.oidcEnabled; }
  get loginProviders() { return this.config.loginProviders; }
  get passwordLoginEnabled() { return this.config.passwordLoginEnabled; }
  get logoUrl() { return this.config.logoUrl; }
  get registrationDisabled() { return this.config.registrationDisabled; }

  loginWithOIDC(): void {
    window.location.href = `${environment.apiUrl}/auth/oidc/login`;
  }

  loginWithProvider(id: string): void {
    window.location.href = `${environment.apiUrl}/auth/providers/${encodeURIComponent(id)}/login`;
  }
}
