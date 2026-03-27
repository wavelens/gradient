/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { DialogModule } from 'primeng/dialog';
import { ButtonModule } from 'primeng/button';
import { InputTextModule } from 'primeng/inputtext';
import { AutoCompleteModule } from 'primeng/autocomplete';
import { OrganizationsService } from '@core/services/organizations.service';
import { CachesService } from '@core/services/caches.service';
import { LoadingSpinnerComponent } from '@shared/components/loading-spinner/loading-spinner.component';

@Component({
  selector: 'app-cache-subscriptions',
  standalone: true,
  imports: [
    CommonModule,
    RouterModule,
    FormsModule,
    DialogModule,
    ButtonModule,
    InputTextModule,
    AutoCompleteModule,
    LoadingSpinnerComponent,
  ],
  templateUrl: './cache-subscriptions.component.html',
  styleUrl: './cache-subscriptions.component.scss',
})
export class CacheSubscriptionsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private orgsService = inject(OrganizationsService);
  private cachesService = inject(CachesService);

  loading = signal(true);
  subscribing = signal(false);
  unsubscribingId = signal<string | null>(null);
  showSubscribeDialog = signal(false);
  errorMessage = signal<string | null>(null);

  orgName = '';
  caches = signal<{ id: string; name: string }[]>([]);
  newCacheName = '';
  cacheSuggestions = signal<string[]>([]);
  private availableCacheNames: string[] = [];

  ngOnInit(): void {
    this.orgName = this.route.snapshot.paramMap.get('org') || '';
    this.loadCaches();
  }

  loadCaches(): void {
    this.loading.set(true);
    this.orgsService.getSubscribedCaches(this.orgName).subscribe({
      next: (list) => {
        this.caches.set(list);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  openSubscribeDialog(): void {
    this.newCacheName = '';
    this.errorMessage.set(null);
    this.cacheSuggestions.set([]);
    this.showSubscribeDialog.set(true);
    this.loadAvailableCaches();
  }

  private loadAvailableCaches(): void {
    const subscribedNames = new Set(this.caches().map((c) => c.name));
    this.cachesService.getCaches().subscribe({
      next: (own) => {
        this.cachesService.getPublicCaches().subscribe({
          next: (pub) => {
            const all = [...own, ...pub];
            const seen = new Set<string>();
            this.availableCacheNames = all
              .filter((c) => !subscribedNames.has(c.name) && !seen.has(c.name) && seen.add(c.name))
              .map((c) => c.name);
          },
          error: () => {
            this.availableCacheNames = own
              .filter((c) => !subscribedNames.has(c.name))
              .map((c) => c.name);
          },
        });
      },
      error: () => {},
    });
  }

  onCacheSearch(event: { query: string }): void {
    const q = event.query.toLowerCase();
    this.cacheSuggestions.set(
      this.availableCacheNames.filter((name) => name.toLowerCase().includes(q))
    );
  }

  subscribeCache(): void {
    const name = this.newCacheName.trim();
    if (!name) return;
    this.subscribing.set(true);
    this.errorMessage.set(null);
    this.orgsService.subscribeCache(this.orgName, name).subscribe({
      next: () => {
        this.subscribing.set(false);
        this.showSubscribeDialog.set(false);
        this.loadCaches();
      },
      error: (err) => {
        this.errorMessage.set(err?.error?.message || err?.message || 'Cache not found or already subscribed.');
        this.subscribing.set(false);
      },
    });
  }

  unsubscribeCache(cache: { id: string; name: string }): void {
    this.unsubscribingId.set(cache.id);
    this.orgsService.unsubscribeCache(this.orgName, cache.name).subscribe({
      next: () => {
        this.unsubscribingId.set(null);
        this.loadCaches();
      },
      error: () => this.unsubscribingId.set(null),
    });
  }
}
