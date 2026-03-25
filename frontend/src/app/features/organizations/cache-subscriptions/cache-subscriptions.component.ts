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
import { OrganizationsService } from '@core/services/organizations.service';
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
    LoadingSpinnerComponent,
  ],
  templateUrl: './cache-subscriptions.component.html',
  styleUrl: './cache-subscriptions.component.scss',
})
export class CacheSubscriptionsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private orgsService = inject(OrganizationsService);

  loading = signal(true);
  subscribing = signal(false);
  unsubscribingId = signal<string | null>(null);
  showSubscribeDialog = signal(false);
  errorMessage = signal<string | null>(null);

  orgName = '';
  caches = signal<{ id: string; name: string }[]>([]);
  newCacheName = '';

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
    this.showSubscribeDialog.set(true);
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
