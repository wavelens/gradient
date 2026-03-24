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
import { InputNumberModule } from 'primeng/inputnumber';
import { TextareaModule } from 'primeng/textarea';
import { CachesService } from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';
import { EmptyStateComponent } from '@shared/components/empty-state/empty-state.component';
import { Cache } from '@core/models';

@Component({
  selector: 'app-cache-list',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    InputNumberModule,
    TextareaModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
  ],
  templateUrl: './cache-list.component.html',
  styleUrl: './cache-list.component.scss',
})
export class CacheListComponent implements OnInit {
  private cachesService = inject(CachesService);

  loading = signal(true);
  caches = signal<Cache[]>([]);
  showCreateDialog = signal(false);
  creating = signal(false);

  newCache = {
    name: '',
    display_name: '',
    description: '',
    priority: 50,
  };

  ngOnInit(): void {
    this.loadCaches();
  }

  loadCaches(): void {
    this.loading.set(true);
    this.cachesService.getCaches().subscribe({
      next: (caches) => {
        this.caches.set(caches);
        this.loading.set(false);
      },
      error: (error) => {
        console.error('Failed to load caches:', error);
        this.loading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.newCache = {
      name: '',
      display_name: '',
      description: '',
      priority: 50,
    };
    this.showCreateDialog.set(true);
  }

  createCache(): void {
    if (!this.newCache.name || !this.newCache.display_name) {
      return;
    }

    this.creating.set(true);
    this.cachesService.createCache(this.newCache).subscribe({
      next: () => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.loadCaches();
      },
      error: (error) => {
        console.error('Failed to create cache:', error);
        this.creating.set(false);
      },
    });
  }

  get activeCaches() {
    return this.caches().filter((c) => c.active);
  }

  get inactiveCaches() {
    return this.caches().filter((c) => !c.active);
  }
}
