/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { ConfirmDialogModule } from 'primeng/confirmdialog';
import { ConfirmationService } from 'primeng/api';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { FormFieldComponent, MessageBannerComponent } from '@shared/components/form';
import { PageLayoutComponent, SettingsSectionComponent } from '@shared/components/layout';

@Component({
  selector: 'app-profile',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    ButtonModule,
    InputTextModule,
    ConfirmDialogModule,
    LoadingSpinnerComponent,
    FormFieldComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    SettingsSectionComponent,
  ],
  providers: [ConfirmationService],
  templateUrl: './profile.component.html',
  styleUrl: './profile.component.scss',
})
export class ProfileComponent implements OnInit {
  private userService = inject(UserService);
  private authService = inject(AuthService);
  private router = inject(Router);
  private confirmService = inject(ConfirmationService);

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);
  isOidc = signal(false);
  isManaged = signal(false);

  errorMessage = signal<string | null>(null);
  successMessage = signal<string | null>(null);

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

  confirmDelete(): void {
    this.confirmService.confirm({
      message: 'Are you sure you want to permanently delete your account? This action cannot be undone.',
      header: 'Delete Account',
      icon: 'pi pi-exclamation-triangle',
      acceptButtonProps: { label: 'Delete Account', severity: 'danger' },
      rejectButtonProps: { label: 'Cancel', severity: 'secondary' },
      accept: () => this.deleteAccount(),
    });
  }

  deleteAccount(): void {
    this.deleting.set(true);
    this.userService.deleteUser().subscribe({
      next: () => {
        this.authService.logout().subscribe();
      },
      error: (error) => {
        console.error('Failed to delete account:', error);
        this.deleting.set(false);
      },
    });
  }
}
