/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

@Component({
  selector: 'app-profile',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    LoadingSpinnerComponent,
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

  showDeleteDialog = signal(false);
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

  deleteAccount(): void {
    this.deleting.set(true);
    this.userService.deleteUser().subscribe({
      next: () => {
        this.authService.logout().subscribe();
      },
      error: (error) => {
        console.error('Failed to delete account:', error);
        this.deleting.set(false);
        this.showDeleteDialog.set(false);
      },
    });
  }
}
