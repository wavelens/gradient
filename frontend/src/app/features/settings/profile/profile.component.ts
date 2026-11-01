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
import { environment } from '@environments/environment';
import {
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  InputDirective,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectButtonComponent,
  SettingsSectionComponent,
} from '@gradient/ui/ui';
import { ManagedDisableDirective } from '@shared/access';
import { AccessState, ConnectedLoginProvider } from '@core/models';

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
    LoadingSpinnerComponent,
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
  theme = inject(ThemeService);

  loading = signal(true);
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
  errorMessage = signal<string | null>(null);
  successMessage = signal<string | null>(null);
  deleteError = signal<string | null>(null);
  deletePassword = '';
  deleteUsernameConfirm = '';

  formData = {
    username: '',
    name: '',
    email: '',
  };

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
        this.successMessage.set('Account disconnected.');
        this.loadSettings();
      },
      error: (error) => {
        this.disconnecting.set(null);
        this.errorMessage.set(error.message || 'Failed to disconnect the account.');
      },
    });
  }

  loadSettings(): void {
    this.loading.set(true);
    this.userService.getUserSettings().subscribe({
      next: (settings) => {
        this.isOidc.set(settings.is_oidc);
        this.isManaged.set(settings.managed);
        this.hasPassword.set(settings.has_password);
        this.loginProviders.set(settings.login_providers);
        this.formData = {
          username: settings.username,
          name: settings.name,
          email: settings.email,
        };
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load user settings:', error);
        this.loading.set(false);
      },
    });
  }

  saveSettings(): void {
    this.saving.set(true);
    this.errorMessage.set(null);
    this.successMessage.set(null);
    this.userService.updateUserSettings(this.formData).subscribe({
      next: () => {
        this.saving.set(false);
        this.successMessage.set('Profile updated successfully.');
        this.authService.reloadUser();
      },
      error: (error) => {
        this.errorMessage.set(error.message || 'Failed to save settings.');
        this.saving.set(false);
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
      error: (error) => {
        this.deleting.set(false);
        this.deleteError.set(error?.message || 'Failed to delete account.');
      },
    });
  }
}
