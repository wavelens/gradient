/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute } from '@angular/router';
import { CachesService } from '@core/services/caches.service';
import { SubscriptionRequest } from '@core/models';
import {
  ButtonComponent,
  EmptyStateComponent,
  LoadingSpinnerComponent,
  MessageBannerComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@shared/ui';

@Component({
  selector: 'app-cache-subscription-requests',
  standalone: true,
  imports: [
    CommonModule,
    ButtonComponent,
    EmptyStateComponent,
    LoadingSpinnerComponent,
    MessageBannerComponent,
    PageLayoutComponent,
    RowComponent,
    RowListComponent,
  ],
  templateUrl: './cache-subscriptions.component.html',
  styleUrl: './cache-subscriptions.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class CacheSubscriptionRequestsComponent implements OnInit {
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);

  cacheName = '';
  loading = signal(true);
  deciding = signal<string | null>(null);
  requests = signal<SubscriptionRequest[]>([]);
  errorMessage = signal<string | null>(null);

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.cachesService.getSubscriptionRequests(this.cacheName).subscribe({
      next: (requests) => {
        this.requests.set(requests);
        this.loading.set(false);
      },
      error: () => {
        this.errorMessage.set('Failed to load subscription requests.');
        this.loading.set(false);
      },
    });
  }

  approve(project: string): void {
    this.deciding.set(project);
    this.cachesService.approveSubscriptionRequest(this.cacheName, project).subscribe({
      next: () => {
        this.deciding.set(null);
        this.load();
      },
      error: (e: Error) => {
        this.deciding.set(null);
        this.errorMessage.set(e.message || 'Failed to approve the request.');
      },
    });
  }

  deny(project: string): void {
    this.deciding.set(project);
    this.cachesService.denySubscriptionRequest(this.cacheName, project).subscribe({
      next: () => {
        this.deciding.set(null);
        this.load();
      },
      error: (e: Error) => {
        this.deciding.set(null);
        this.errorMessage.set(e.message || 'Failed to deny the request.');
      },
    });
  }
}
