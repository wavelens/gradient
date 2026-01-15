/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { UserService } from '@core/services/user.service';
import { ApiKey } from '@core/models';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

@Component({
  selector: 'app-api-keys',
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
  templateUrl: './api-keys.component.html',
  styleUrl: './api-keys.component.scss',
})
export class ApiKeysComponent implements OnInit {
  private userService = inject(UserService);

  loading = signal(true);
  creating = signal(false);
  deletingId = signal<string | null>(null);

  keys = signal<ApiKey[]>([]);
  showCreateDialog = signal(false);
  showKeyDialog = signal(false);
  newKeyName = '';
  createdKeyValue = signal('');
  errorMessage = signal<string | null>(null);

  ngOnInit(): void {
    this.loadKeys();
  }

  loadKeys(): void {
    this.loading.set(true);
    this.userService.getApiKeys().subscribe({
      next: (keys) => {
        this.keys.set(keys);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load API keys:', error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.newKeyName = '';
    this.errorMessage.set(null);
    this.showCreateDialog.set(true);
  }

  createKey(): void {
    const name = this.newKeyName.trim();
    if (!name) return;

    this.creating.set(true);
    this.errorMessage.set(null);
    this.userService.createApiKey(name).subscribe({
      next: (keyValue) => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.createdKeyValue.set(keyValue);
        this.showKeyDialog.set(true);
        this.loadKeys();
      },
      error: (error) => {
        this.errorMessage.set(error.message || 'Failed to create API key.');
        this.creating.set(false);
      },
    });
  }

  deleteKey(key: ApiKey): void {
    this.deletingId.set(key.id);
    this.userService.deleteApiKey(key.name).subscribe({
      next: () => {
        this.deletingId.set(null);
        this.loadKeys();
      },
      error: (error) => {
        console.error('Failed to delete API key:', error);
        this.deletingId.set(null);
      },
    });
  }

  copyKey(): void {
    navigator.clipboard.writeText(this.createdKeyValue());
  }
}
