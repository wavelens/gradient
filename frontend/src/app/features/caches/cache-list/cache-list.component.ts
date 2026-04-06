/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnDestroy, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { Subject, EMPTY } from 'rxjs';
import { debounceTime, switchMap } from 'rxjs/operators';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { TextareaModule } from 'primeng/textarea';
import { CachesService } from '@core/services/caches.service';
import { AuthService } from '@core/services/auth.service';
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
    TextareaModule,
    LoadingSpinnerComponent,
    EmptyStateComponent,
  ],
  templateUrl: './cache-list.component.html',
  styleUrl: './cache-list.component.scss',
})
export class CacheListComponent implements OnInit, OnDestroy {
  private cachesService = inject(CachesService);
  protected authService = inject(AuthService);
  private nameCheck$ = new Subject<string>();

  loading = signal(true);
  caches = signal<Cache[]>([]);
  showCreateDialog = signal(false);
  creating = signal(false);
  createError = signal<string | null>(null);
  nameCheckState = signal<'idle' | 'invalid' | 'checking' | 'available' | 'taken'>('idle');

  newCache = {
    name: '',
    display_name: '',
    description: '',
    priority: 50,
    public: false,
  };

  publicCaches = signal<Cache[]>([]);
  publicLoading = signal(false);

  get priorityInvalid(): boolean {
    const p = this.newCache.priority;
    return p === null || p === undefined || isNaN(Number(p)) || p < 0 || p > 255;
  }

  ngOnInit(): void {
    if (this.authService.isAuthenticated()) {
      this.loadCaches();
    } else {
      this.loading.set(false);
    }
    this.loadPublicCaches();
    this.nameCheck$.pipe(
      debounceTime(400),
      switchMap((name) => name ? this.cachesService.checkCacheNameAvailable(name) : EMPTY),
    ).subscribe((available) => {
      this.nameCheckState.set(available ? 'available' : 'taken');
    });
  }

  ngOnDestroy(): void {
    this.nameCheck$.complete();
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

  loadPublicCaches(): void {
    this.publicLoading.set(true);
    this.cachesService.getPublicCaches().subscribe({
      next: (caches) => {
        this.publicCaches.set(caches);
        this.publicLoading.set(false);
      },
      error: () => this.publicLoading.set(false),
    });
  }

  openCreateDialog(): void {
    this.newCache = { name: '', display_name: '', description: '', priority: 50, public: false };
    this.nameCheckState.set('idle');
    this.createError.set(null);
    this.showCreateDialog.set(true);
  }

  onCacheNameChange(name: string): void {
    if (!name) { this.nameCheckState.set('idle'); this.nameCheck$.next(''); return; }
    if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/.test(name)) {
      this.nameCheckState.set('invalid');
      this.nameCheck$.next(''); // cancel any pending debounce without making an API call
      return;
    }
    this.nameCheckState.set('checking');
    this.nameCheck$.next(name);
  }

  createCache(): void {
    if (!this.newCache.name || !this.newCache.display_name) {
      return;
    }

    this.creating.set(true);
    this.createError.set(null);
    this.cachesService.createCache(this.newCache).subscribe({
      next: () => {
        this.creating.set(false);
        this.showCreateDialog.set(false);
        this.loadCaches();
      },
      error: (error) => {
        this.createError.set(error?.message || 'Failed to create cache.');
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

  get filteredPublicCaches(): Cache[] {
    const ownedIds = new Set(this.caches().map((c) => c.id));
    return this.publicCaches().filter((c) => !ownedIds.has(c.id));
  }
}
