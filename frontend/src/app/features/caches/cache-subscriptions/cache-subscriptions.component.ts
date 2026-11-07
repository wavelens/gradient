/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
import { NotifyService } from '@core/services/notify.service';
import { SubscriptionRequestItem } from '@core/models';
import {
  ButtonComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-cache-subscription-requests',
  standalone: true,
  imports: [
    CommonModule,
    ButtonComponent,
    LoadStateComponent,
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
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cacheSettings(this.cacheName, { label: 'Subscriptions' }));
  loading = signal(true);
  deciding = signal<string | null>(null);
  requests = signal<SubscriptionRequestItem[]>([]);
  loadError = signal<unknown>(null);

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.cachesService.getSubscriptionRequests(this.cacheName).subscribe({
      next: (requests) => {
        this.requests.set(requests);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  approve(project: string): void {
    this.deciding.set(project);
    this.cachesService.approveSubscriptionRequest(this.cacheName, project).subscribe({
      next: () => {
        this.deciding.set(null);
        this.notify.success('Approved subscription request');
        this.load();
      },
      error: (error: unknown) => {
        this.deciding.set(null);
        this.notify.failure('Failed to approve subscription request', error);
      },
    });
  }

  deny(project: string): void {
    this.deciding.set(project);
    this.cachesService.denySubscriptionRequest(this.cacheName, project).subscribe({
      next: () => {
        this.deciding.set(null);
        this.notify.success('Denied subscription request');
        this.load();
      },
      error: (error: unknown) => {
        this.deciding.set(null);
        this.notify.failure('Failed to deny subscription request', error);
      },
    });
  }
}
