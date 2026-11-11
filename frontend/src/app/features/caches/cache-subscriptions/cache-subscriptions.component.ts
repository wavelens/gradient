/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { EntityNameComponent, entityKindToIconName, formatEntityLabel } from '@shared/ui';
import { ServerTimestampToLocalDateTimePipe, getSubscriptionModesWithNoMoreAccessThan, subscriptionModeLabel } from '@shared/text';
import { CommonModule } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute } from '@angular/router';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { CachesService } from '@core/services/caches.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import { CacheSubscriptionMode, SubscriptionRequestItem } from '@core/models';
import { ConfirmService } from '@shared/confirm';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { tap } from 'rxjs';
import { requestIfCacheMember } from '@shared/access';
import { injectCacheAccessData } from '@core/resolvers/inject-access';
import {
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  FormFieldComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  SubmitDirective,
} from '@gradient/ui/ui';

@Component({
  selector: 'app-cache-subscription-requests',
  standalone: true,
  imports: [
    EntityNameComponent,
    CommonModule,
    FormsModule,
    ServerTimestampToLocalDateTimePipe,
    BadgeComponent,
    ButtonComponent,
    DialogComponent,
    FormFieldComponent,
    SelectComponent,
    SubmitDirective,
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
  protected readonly entityKindToIconName = entityKindToIconName;
  private route = inject(ActivatedRoute);
  private cachesService = inject(CachesService);
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);
  private cacheAccess = injectCacheAccessData();

  cacheName = '';
  breadcrumb = computed(() => this.crumbs.cacheSettings(this.cacheName, { label: 'Subscriptions' }));
  loading = signal(true);
  deciding = signal<string | null>(null);
  requests = signal<SubscriptionRequestItem[]>([]);
  loadError = signal<unknown>(null);
  subscriptionRequestInApproveDialog = signal<SubscriptionRequestItem | null>(null);
  approvedSubscriptionMode = signal<CacheSubscriptionMode>('ReadWrite');
  approveSubscriptionRequestErrorMessage = signal<string | null>(null);
  subscriptionModesWithNoMoreAccessThanRequested = computed(() => {
    const request = this.subscriptionRequestInApproveDialog();
    return request ? getSubscriptionModesWithNoMoreAccessThan(request.mode) : [];
  });
  protected readonly modeLabel = subscriptionModeLabel;

  constructor() {
    runReloadIntervalWhileTabVisible(() =>
      requestIfCacheMember(this.cacheAccess()?.cache, () => this.cachesService.getSubscriptionRequests(this.cacheName), 'manageCacheSubscriptions').pipe(tap((requests) => {
        this.requests.set(requests);
        this.loadError.set(null);
      })));
  }

  ngOnInit(): void {
    this.cacheName = this.route.snapshot.paramMap.get('cache') || '';
    this.load();
  }

  load(): void {
    this.loading.set(true);
    this.loadError.set(null);
    requestIfCacheMember(this.cacheAccess()?.cache, () => this.cachesService.getSubscriptionRequests(this.cacheName), 'manageCacheSubscriptions').subscribe({
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

  approve(request: SubscriptionRequestItem): void {
    if (request.mode !== 'ReadOnly') {
      this.approvedSubscriptionMode.set(request.mode);
      this.approveSubscriptionRequestErrorMessage.set(null);
      this.subscriptionRequestInApproveDialog.set(request);
      return;
    }
    this.deciding.set(request.project);
    this.cachesService.approveSubscriptionRequest(this.cacheName, request.project).subscribe({
      next: () => {
        this.deciding.set(null);
        this.onApproved();
      },
      error: (error: unknown) => {
        this.deciding.set(null);
        this.notify.failure("Couldn't approve the subscription request", error);
      },
    });
  }

  approveOpenRequestWithSelectedMode(): void {
    const request = this.subscriptionRequestInApproveDialog();
    if (!request || this.deciding()) return;
    this.deciding.set(request.project);
    this.approveSubscriptionRequestErrorMessage.set(null);
    this.cachesService.approveSubscriptionRequest(this.cacheName, request.project, this.approvedSubscriptionMode()).subscribe({
      next: () => {
        this.deciding.set(null);
        this.subscriptionRequestInApproveDialog.set(null);
        this.onApproved();
      },
      error: (error: unknown) => {
        this.deciding.set(null);
        this.approveSubscriptionRequestErrorMessage.set(apiErrorMessage(error, "Couldn't approve the subscription request"));
      },
    });
  }

  closeApproveDialogWhenVisibilityIsHidden(visible: boolean): void {
    if (!visible) this.subscriptionRequestInApproveDialog.set(null);
  }

  private onApproved(): void {
    this.notify.success('Approved subscription request');
    this.load();
  }

  deny(request: SubscriptionRequestItem): void {
    const project = formatEntityLabel({ display_name: request.project_display_name, name: request.project });
    this.confirm
      .confirm({
        title: 'Deny subscription request',
        target: project,
        consequence: `${project} can't use this cache. The project can apply again later.`,
        confirmLabel: 'Deny',
        failure: 'Failed to deny the subscription request.',
        action: () => this.cachesService.denySubscriptionRequest(this.cacheName, request.project),
      })
      .subscribe(() => {
        this.notify.success('Denied subscription request');
        this.load();
      });
  }
}
