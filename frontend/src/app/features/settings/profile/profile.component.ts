/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { DividerModule } from 'primeng/divider';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { ManagedDisableDirective } from '@shared/access';
import { AccessState } from '@core/models';

@Component({
  selector: 'app-profile',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    DividerModule,
    ButtonModule,
    InputTextModule,
    LoadingSpinnerComponent,
    ManagedDisableDirective,
  ],
  templateUrl: './profile.component.html',
  styleUrl: './profile.component.scss',
})
export class ProfileComponent implements OnInit {
  private userService = inject(UserService);
  private authService = inject(AuthService);
  private router = inject(Router);

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  isOidc = signal(false);
  isManaged = signal(false);

  /** OIDC and Nix-managed both freeze the profile fields; the user always
   * retains the right to edit their own account in principle, so canEdit
   * stays true and the banner / disable behavior comes from `managed`. */
  access = computed<AccessState>(() => ({
    managed: this.isManaged() || this.isOidc(),
    canEdit: true,
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

  ngOnInit(): void {
    this.loadSettings();
  }

  loadSettings(): void {
    this.loading.set(true);
    this.userService.getUserSettings().subscribe({
      next: (settings) => {
        this.isOidc.set(settings.is_oidc);
        this.isManaged.set(settings.managed);
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
    const body = this.isOidc()
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
