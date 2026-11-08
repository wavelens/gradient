/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { ThemeService, ThemePreference } from '@core/services/theme.service';
import { apiErrorMessage } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { environment } from '@environments/environment';
import {
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadStateComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectButtonComponent,
  SettingsSectionComponent,
  SubmitDirective,
} from '@gradient/ui/ui';
import { ManagedDisableDirective } from '@shared/access';
import { unsavedChanges } from '@shared/forms';
import { CanComponentDeactivate } from '@core/guards/unsaved-changes.guard';
import { AccessState, ConnectedLoginProvider, PatchUserSettingsRequest } from '@core/models';
import { links } from '@shared/routing';
import { SettingsNavComponent } from '../settings-nav/settings-nav.component';

interface ProfileForm {
  username: string;
  name: string;
  email: string;
}

const USERNAME = /^[A-Za-z0-9][A-Za-z0-9_-]{1,48}[A-Za-z0-9]$/;
const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

const FIELD_CHECKS: Record<keyof ProfileForm, (value: string) => string | null> = {
  username: (value) => {
    if (!value) return 'Enter a username.';
    return USERNAME.test(value)
      ? null
      : 'Use 3-50 characters. Allowed characters are letters, digits, hyphens and underscore characters. Start and end with a letter or digit.';
  },
  name: (value) => (value ? null : 'Enter a full name.'),
  email: (value) => {
    if (!value) return 'Enter an email address.';
    return EMAIL.test(value) ? null : 'Enter an email address such as name@example.com.';
  },
};

const FIELDS = Object.keys(FIELD_CHECKS) as (keyof ProfileForm)[];

const CONNECT_ERRORS: Record<string, string> = {
  taken: 'This account is already connected to another user.',
  already_connected: 'An account of this provider is already connected.',
  signed_out: 'You signed out before the provider answered. Sign in again and retry.',
  failed: 'Connecting the account failed. Please try again.',
};

@Component({
  selector: 'app-profile',
  standalone: true,
  imports: [
    SettingsNavComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    ButtonComponent,
    InputDirective,
    LoadStateComponent,
    ManagedDisableDirective,
    PageLayoutComponent,
    FormFieldComponent,
    SettingsSectionComponent,
    MessageBannerComponent,
    RowComponent,
    RowListComponent,
    SelectButtonComponent,
    SubmitDirective,
  ],
  templateUrl: './profile.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styles: `
    .save-hint {
      align-self: center;
    }
  `,
})
export class ProfileComponent implements OnInit, CanComponentDeactivate {
  protected readonly links = links;
  private userService = inject(UserService);
  private authService = inject(AuthService);
  private router = inject(Router);
  private route = inject(ActivatedRoute);
  private notify = inject(NotifyService);
  theme = inject(ThemeService);

  loading = signal(true);
  loadError = signal<unknown>(null);
  saving = signal(false);
  deleting = signal(false);
  isOidc = signal(false);
  isManaged = signal(false);
  hasPassword = signal(true);
  loginProviders = signal<ConnectedLoginProvider[]>([]);
  disconnecting = signal<string | null>(null);

  /** OIDC and Nix-managed both freeze the profile fields; the user always
   * retains the right to edit their own account in principle, so canEdit
   * stays true and the banner / disable behavior comes from `managed`. */
  access = computed<AccessState>(() => ({
    managed: this.isManaged() || this.isOidc(),
    canEdit: true,
    canTrigger: true,
  }));

  showDeleteDialog = signal(false);
  savedUsername = signal('');
  errorMessage = signal<string | null>(null);
  successMessage = signal<string | null>(null);
  deleteError = signal<string | null>(null);
  deletePassword = '';
  deleteUsernameConfirm = '';

  formData: ProfileForm = { username: '', name: '', email: '' };
  private saved: ProfileForm = { ...this.formData };
  readonly unsaved = unsavedChanges(() => this.formData, 'Profile settings');

  themeOptions: readonly { label: string; value: ThemePreference }[] = [
    { label: 'System', value: 'system' },
    { label: 'Light', value: 'light' },
    { label: 'Dark', value: 'dark' },
  ];

  setTheme(preference: ThemePreference): void {
    this.theme.set(preference);
  }

  ngOnInit(): void {
    this.showConnectOutcome();
    this.loadSettings();
  }

  private showConnectOutcome(): void {
    const query = this.route.snapshot.queryParamMap;
    if (query.get('connected')) {
      this.successMessage.set('Account connected.');
    }
    const error = query.get('connect_error');
    if (error) {
      this.errorMessage.set(CONNECT_ERRORS[error] ?? CONNECT_ERRORS['failed']);
    }
  }

  connectProvider(id: string): void {
    window.location.href = `${environment.apiUrl}/user/identities/${encodeURIComponent(id)}/connect`;
  }

  disconnectProvider(id: string): void {
    this.disconnecting.set(id);
    this.errorMessage.set(null);
    this.successMessage.set(null);
    this.userService.disconnectLoginProvider(id).subscribe({
      next: () => {
        this.disconnecting.set(null);
        this.notify.success('Account disconnected');
        this.loadSettings();
      },
      error: (error: unknown) => {
        this.disconnecting.set(null);
        this.notify.failure('Failed to disconnect account', error);
      },
    });
  }

  loadSettings(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.userService.getUserSettings().subscribe({
      next: (settings) => {
        this.isOidc.set(settings.is_oidc);
        this.isManaged.set(settings.managed);
        this.hasPassword.set(settings.has_password);
        this.loginProviders.set(settings.login_providers);
        this.savedUsername.set(settings.username);
        this.saved = { username: settings.username, name: settings.name, email: settings.email };
        this.formData = { ...this.saved };
        this.unsaved.mark();
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  canDeactivate() {
    return this.unsaved.canLeave();
  }

  fieldError(field: keyof ProfileForm): string | null {
    const value = this.formData[field].trim();
    return value === this.saved[field] ? null : FIELD_CHECKS[field](value);
  }

  canSave(): boolean {
    return !this.saving() && !this.access().managed && this.unsaved.changed() && FIELDS.every((field) => !this.fieldError(field));
  }

  saveSettings(): void {
    if (!this.canSave()) return;
    const values: ProfileForm = {
      username: this.formData.username.trim(),
      name: this.formData.name.trim(),
      email: this.formData.email.trim(),
    };
    const patch: PatchUserSettingsRequest = {};
    for (const field of FIELDS) {
      if (values[field] !== this.saved[field]) patch[field] = values[field];
    }
    const settle = () => {
      this.saved = values;
      this.formData = { ...values };
      this.savedUsername.set(values.username);
      this.unsaved.mark();
    };
    if (Object.keys(patch).length === 0) return settle();
    this.saving.set(true);
    this.errorMessage.set(null);
    this.successMessage.set(null);
    this.userService.updateUserSettings(patch).subscribe({
      next: () => {
        this.saving.set(false);
        settle();
        this.notify.success('Profile saved');
        this.authService.reloadUser();
      },
      error: (error: unknown) => {
        this.saving.set(false);
        this.notify.failure('Failed to save profile', error);
      },
    });
  }

  openDeleteDialog(): void {
    this.deletePassword = '';
    this.deleteUsernameConfirm = '';
    this.deleteError.set(null);
    this.showDeleteDialog.set(true);
  }

  deleteAccount(): void {
    this.deleting.set(true);
    this.deleteError.set(null);
    const body = !this.hasPassword()
      ? { confirm_username: this.deleteUsernameConfirm }
      : { password: this.deletePassword };
    this.userService.deleteUser(body).subscribe({
      next: () => {
        this.authService.logout().subscribe();
      },
      error: (error: unknown) => {
        this.deleting.set(false);
        this.deleteError.set(apiErrorMessage(error, 'Failed to delete account.'));
      },
    });
  }
}
