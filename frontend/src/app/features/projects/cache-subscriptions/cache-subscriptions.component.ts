/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  TooltipDirective,
} from '@gradient/ui/ui';
import { LabelHelpComponent } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, requestIfProjectMember } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { CreateCacheButtonComponent, CreateCacheDialogComponent, injectCanCreateCache } from '@shared/forms';
import { accessTo, Cache, CacheSubscriptionItem } from '@core/models';
import { links } from '@shared/routing';
import { catchError, forkJoin, map, of } from 'rxjs';

@Component({
  selector: 'app-cache-subscriptions',
  standalone: true,
  imports: [
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    BadgeComponent,
    ButtonComponent,
    AutoCompleteComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective, ServerConfigurationManagedBannerComponent,
    PageLayoutComponent,
    FormFieldComponent,
    EmptyStateComponent,
    RowListComponent,
    RowComponent,
    TooltipDirective,
    CreateCacheButtonComponent,
    CreateCacheDialogComponent,
  ],
  templateUrl: './cache-subscriptions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class CacheSubscriptionsComponent implements OnInit {
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private projectsService = inject(ProjectsService);
  private cachesService = inject(CachesService);
  private resolvedProject = injectProject();
  private crumbs = inject(BreadcrumbsService);
  private auth = inject(AuthService);
  private config = inject(ConfigService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = computed(() => accessTo(this.resolvedProject(), 'manageSubscriptions'));
  canCreateCache = computed(() => this.config.canCreate(this.config.createCache, this.auth.user()?.superuser === true));
  private userCanCreateCache = injectCanCreateCache();
  canCreateAndSubscribe = computed(() => this.userCanCreateCache() && this.access().canEdit && !this.access().managed);
  showCreateDialog = signal(false);
  createForSubscribe = signal(false);

  loading = signal(true);
  loadError = signal<unknown>(null);
  subscribing = signal(false);
  showSubscribeDialog = signal(false);
  errorMessage = signal<string | null>(null);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Cache Subscriptions' }));
  caches = signal<CacheSubscriptionItem[]>([]);
  newCacheName = '';
  cacheSuggestions = signal<string[]>([]);
  private availableCaches: Cache[] = [];

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    this.loadCaches();
    const preselected = this.route.snapshot.queryParamMap.get('subscribe');
    if (preselected) this.openSubscribeDialog(preselected);
  }

  loadCaches(): void {
    this.loading.set(true);
    this.loadError.set(null);
    requestIfProjectMember(this.resolvedProject(), () => this.projectsService.getSubscribedCaches(this.projectName)).subscribe({
      next: (list) => {
        this.caches.set(list);
        this.loading.set(false);
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  openSubscribeDialog(cache = ''): void {
    this.newCacheName = cache;
    this.errorMessage.set(null);
    this.cacheSuggestions.set([]);
    this.showSubscribeDialog.set(true);
    this.loadAvailableCaches();
  }

  private loadAvailableCaches(): void {
    const subscribedNames = new Set(this.caches().map((c) => c.name));
    forkJoin([
      this.cachesService.getCaches(1, 100).pipe(map((own) => own.items), catchError(() => of([]))),
      this.cachesService.getPublicCaches().pipe(catchError(() => of([]))),
    ]).subscribe(([own, pub]) => {
      const seen = new Set<string>();
      this.availableCaches = [...own, ...pub].filter((c) => !subscribedNames.has(c.name) && !seen.has(c.name) && seen.add(c.name));
      if (this.showSubscribeDialog()) this.onCacheSearch({ query: this.newCacheName });
    });
  }

  onCacheSearch(event: { query: string }): void {
    const q = event.query.trim().toLowerCase();
    this.cacheSuggestions.set(
      this.availableCaches
        .filter((c) => c.name.toLowerCase().includes(q) || c.display_name.toLowerCase().includes(q))
        .map((c) => c.name),
    );
  }

  createInsteadOfPick(): void {
    this.showSubscribeDialog.set(false);
    this.createForSubscribe.set(true);
    this.showCreateDialog.set(true);
  }

  cacheCreated(name: string): void {
    if (this.createForSubscribe()) {
      this.createForSubscribe.set(false);
      this.openSubscribeDialog(name);
      return;
    }
    this.newCacheName = name;
    this.subscribeCache();
  }

  createDialogClosed(visible: boolean): void {
    if (visible || !this.createForSubscribe()) return;
    this.createForSubscribe.set(false);
    this.showSubscribeDialog.set(true);
  }

  subscribeCache(): void {
    const name = this.newCacheName.trim();
    if (!name || this.subscribing()) return;
    this.subscribing.set(true);
    this.errorMessage.set(null);
    this.projectsService.subscribeCache(this.projectName, name).subscribe({
      next: () => {
        this.subscribing.set(false);
        this.showSubscribeDialog.set(false);
        this.notify.success('Subscribed to cache');
        this.loadCaches();
      },
      error: (error: unknown) => {
        this.errorMessage.set(apiErrorMessage(error, 'Cache not found or already subscribed.'));
        this.subscribing.set(false);
        this.showSubscribeDialog.set(true);
      },
    });
  }

  unsubscribeCache(cache: CacheSubscriptionItem): void {
    const pending = cache.status === 'pending';
    this.confirm
      .confirm({
        title: pending ? 'Withdraw Subscription Request' : 'Unsubscribe from Cache',
        target: cache.name,
        consequence: pending
          ? `Delete the subscription request to ${cache.name}.`
          : `Builds for this project no longer use ${cache.name}.`,
        confirmLabel: pending ? 'Withdraw' : 'Unsubscribe',
        failure: 'Failed to delete the subscription.',
        action: () => this.projectsService.unsubscribeCache(this.projectName, cache.name),
      })
      .subscribe(() => {
        this.notify.success('Removed cache subscription');
        this.loadCaches();
      });
  }
}
