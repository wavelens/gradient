/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { EntityNameComponent, entityKindToIconName, formatEntityLabel } from '@shared/ui';
import { CommonModule } from '@angular/common';
import { Router } from '@angular/router';
import { CachesService } from '@core/services/caches.service';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import {
  BadgeComponent,
  CardGridComponent,
  LoadMoreComponent,
  EmptyStateComponent,
  LoadStateComponent,
  NavCardComponent,
  PageLayoutComponent,
} from '@gradient/ui/ui';
import { CreateCacheButtonComponent, CreateCacheDialogComponent, openCreateDialogFromUrlIfAllowed } from '@shared/forms';
import { Cache } from '@core/models';
import { links } from '@shared/routing';
import { PendingInvitationsBannerComponent } from '@shared/access';
import { pagedList, runReloadIntervalWhileTabVisible } from '@shared/paging';
import { EMPTY, catchError, merge, tap } from 'rxjs';

@Component({
  selector: 'app-cache-list',
  standalone: true,
  imports: [
    EntityNameComponent,
    CommonModule,
    LoadStateComponent,
    EmptyStateComponent,
    PageLayoutComponent,
    NavCardComponent,
    CardGridComponent,
    LoadMoreComponent,
    BadgeComponent,
    CreateCacheButtonComponent,
    CreateCacheDialogComponent,
    PendingInvitationsBannerComponent,
  ],
  templateUrl: './cache-list.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './cache-list.component.scss',
})
export class CacheListComponent implements OnInit {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly formatEntityLabel = formatEntityLabel;
  protected readonly links = links;
  private cachesService = inject(CachesService);
  protected authService = inject(AuthService);
  private config = inject(ConfigService);
  private router = inject(Router);

  get canCreateCache(): boolean {
    if (!this.authService.isAuthenticated()) return false;
    return this.config.canCreate(this.config.createCache, this.authService.user()?.superuser === true);
  }

  caches = pagedList<Cache>('caches', (page) => this.cachesService.getCaches(page));
  showCreateDialog = signal(false);

  publicCaches = signal<Cache[]>([]);
  publicLoading = signal(false);
  publicError = signal<unknown>(null);

  get showPublic(): boolean {
    return this.config.publicListing;
  }

  constructor() {
    runReloadIntervalWhileTabVisible(() =>
      merge(
        this.authService.isAuthenticated() ? this.caches.refreshFirstPageSilently().pipe(catchError(() => EMPTY)) : EMPTY,
        this.config.publicListing ? this.refreshPublicCachesSilently() : EMPTY,
      ),
    );
    openCreateDialogFromUrlIfAllowed(() => this.canCreateCache, () => this.openCreateDialog());
  }

  ngOnInit(): void {
    if (this.authService.isAuthenticated()) this.caches.reload();
    if (this.config.publicListing) {
      this.loadPublicCaches();
    }
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

  private refreshPublicCachesSilently() {
    return this.cachesService.getPublicCaches().pipe(
      tap((caches) => {
        this.publicCaches.set(caches);
        this.publicError.set(null);
      }),
      catchError(() => EMPTY),
    );
  }

  openCreateDialog(): void {
    this.showCreateDialog.set(true);
  }

  openCreatedCache(name: string): void {
    this.router.navigateByUrl(links.cache(name));
  }

  get activeCaches() {
    return this.caches.items().filter((c) => c.active);
  }

  get inactiveCaches() {
    return this.caches.items().filter((c) => !c.active);
  }

  get filteredPublicCaches(): Cache[] {
    const ownedIds = new Set(this.caches.items().map((c) => c.id));
    return this.publicCaches().filter((c) => !ownedIds.has(c.id));
  }
}
