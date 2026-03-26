/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { CachesService } from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { Cache } from '@core/models';

@Component({
  selector: 'app-cache-settings',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    TextareaModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './cache-settings.component.html',
  styleUrl: './cache-settings.component.scss',
})
export class CacheSettingsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private cachesService = inject(CachesService);

  loading = signal(true);
  saving = signal(false);
  deleting = signal(false);

  cache = signal<Cache | null>(null);
  showDeleteDialog = signal(false);

  cacheName = '';

  formData = {
    display_name: '',
    description: '',
    priority: 50,
    public: false,
  };

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.loadCache();
  }

  loadCache(): void {
    this.loading.set(true);
    this.cachesService.getCache(this.cacheName).subscribe({
      next: (cache) => {
        this.cache.set(cache);
        this.formData = {
          display_name: cache.display_name,
          description: cache.description,
          priority: cache.priority,
          public: cache.public,
        };
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load cache:', error);
        this.loading.set(false);
      },
    });
  }

  get priorityInvalid(): boolean {
    const p = this.formData.priority;
    return p === null || p === undefined || isNaN(Number(p)) || p < 0 || p > 255;
  }

  saveSettings(): void {
    if (this.priorityInvalid) return;
    this.saving.set(true);

    const visibilityCall = this.formData.public
      ? this.cachesService.setCachePublic(this.cacheName)
      : this.cachesService.setCachePrivate(this.cacheName);

    this.cachesService.updateCache(this.cacheName, {
      display_name: this.formData.display_name,
      description: this.formData.description,
      priority: this.formData.priority,
    }).subscribe({
      next: () => {
        visibilityCall.subscribe({
          next: () => {
            this.saving.set(false);
            this.loadCache();
          },
          error: (error) => {
            console.error('Failed to update visibility:', error);
            this.saving.set(false);
            this.loadCache();
          },
        });
      },
      error: (error) => {
        console.error('Failed to save settings:', error);
        this.saving.set(false);
      },
    });
  }

  deleteCache(): void {
    this.deleting.set(true);
    this.cachesService.deleteCache(this.cacheName).subscribe({
      next: () => {
        this.router.navigate(['/caches']);
      },
      error: (error) => {
        console.error('Failed to delete cache:', error);
        this.deleting.set(false);
        this.showDeleteDialog.set(false);
      },
    });
  }
}
