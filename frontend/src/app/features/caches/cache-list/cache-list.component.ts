/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { RouterModule } from '@angular/router';
import { CachesService } from '@core/services/caches.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import {
  BadgeComponent,
  CardGridComponent,
  EmptyStateComponent,
  IconComponent,
  LoadStateComponent,
  NavCardComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { CreateCacheButtonComponent, CreateCacheDialogComponent } from '@shared/forms';
import { Cache } from '@core/models';
import { links } from '@shared/routing';

@Component({
  selector: 'app-cache-list',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    LoadStateComponent,
    EmptyStateComponent,
    IconComponent,
    PageLayoutComponent,
    NavCardComponent,
    CardGridComponent,
    BadgeComponent,
    CreateCacheButtonComponent,
    CreateCacheDialogComponent,
  ],
  templateUrl: './cache-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache-list.component.scss',
})
export class CacheListComponent implements OnInit {
  protected readonly links = links;
  private cachesService = inject(CachesService);
  protected authService = inject(AuthService);
  private config = inject(ConfigService);

  get canCreateCache(): boolean {
    if (!this.authService.isAuthenticated()) return false;
    return this.config.canCreate(this.config.createCache, this.authService.user()?.superuser === true);
  }

  loading = signal(true);
  loadError = signal<unknown>(null);
  caches = signal<Cache[]>([]);
  showCreateDialog = signal(false);

  publicCaches = signal<Cache[]>([]);
  publicLoading = signal(false);
  publicError = signal<unknown>(null);

  get showPublic(): boolean {
    return this.config.publicListing;
  }

  ngOnInit(): void {
    if (this.authService.isAuthenticated()) {
      this.loadCaches();
    } else {
      this.loading.set(false);
    }
    if (this.config.publicListing) {
      this.loadPublicCaches();
    }
  }

  loadCaches(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.cachesService.getCaches(1, 100).subscribe({
      next: (caches) => {
        this.caches.set(caches.items);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  loadPublicCaches(): void {
    this.publicLoading.set(true);
    this.publicError.set(null);
    this.cachesService.getPublicCaches().subscribe({
      next: (caches) => {
        this.publicCaches.set(caches);
        this.publicLoading.set(false);
      },
      error: (error: unknown) => {
        this.publicError.set(error);
        this.publicLoading.set(false);
      },
    });
  }

  openCreateDialog(): void {
    this.showCreateDialog.set(true);
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
