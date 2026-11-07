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
import { ConfigService } from '@core/services/config.service';
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
} from '@gradient/ui/ui';
import { ManagedDisableDirective } from '@shared/access';
import { AccessState, ConnectedLoginProvider } from '@core/models';

interface ProfileFields {
  username: string;
  name: string;
  email: string;
}

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
  ],
  templateUrl: './profile.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProfileComponent implements OnInit {
  private userService = inject(UserService);
  private authService = inject(AuthService);
  protected config = inject(ConfigService);
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

  formData: ProfileFields = {
    username: '',
    name: '',
    email: '',
  };

  private savedProfile: ProfileFields = { ...this.formData };

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
        this.formData = {
          username: settings.username,
          name: settings.name,
          email: settings.email,
        };
        this.savedProfile = { ...this.formData };
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  saveSettings(): void {
    this.errorMessage.set(null);
    this.successMessage.set(null);
    const changes = this.changedProfileFields();
    if (Object.keys(changes).length === 0) {
      this.successMessage.set('Profile updated successfully.');
      return;
    }
    this.saving.set(true);
    this.userService.updateUserSettings(changes).subscribe({
      next: () => {
        this.savedProfile = { ...this.savedProfile, ...changes };
        this.saving.set(false);
        this.notify.success('Profile saved');
        this.authService.reloadUser();
      },
      error: (error: unknown) => {
        this.saving.set(false);
        this.notify.failure('Failed to save profile', error);
      },
    });
  }

  private changedProfileFields(): Partial<ProfileFields> {
    const fields = Object.keys(this.formData) as (keyof ProfileFields)[];
    return Object.fromEntries(
      fields
        .filter((field) => this.formData[field] !== this.savedProfile[field])
        .map((field) => [field, this.formData[field]]),
    );
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
