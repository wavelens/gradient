/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, OnInit, computed, inject, signal, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { ActivatedRoute, Router, RouterModule } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { injectProject } from '@core/resolvers/project-access.resolver';
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
  type AutoCompleteSuggestion,
} from '@gradient/ui/ui';
import { EntityNameComponent, LabelHelpComponent, doesSearchQueryMatchEntityNameOrSlug, entityKindToIconName, entityToPickerOption, formatEntityLabel } from '@shared/ui';
import { WritableDirective, ManagedDisableDirective, ServerConfigurationManagedBannerComponent, requestIfProjectMember } from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { CreateCacheDialogComponent, injectCanCreateCache } from '@shared/forms';
import { accessTo, Cache, CacheSubscriptionItem } from '@core/models';
import { links } from '@shared/routing';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { ProjectNoActiveWritableCacheSubscriptionWarningComponent } from '../project-no-active-writable-cache-subscription-warning.component';
import { EMPTY, Observable, catchError, forkJoin, map, of, tap } from 'rxjs';

@Component({
  selector: 'app-cache-subscriptions',
  standalone: true,
  imports: [
    EntityNameComponent,
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
    CreateCacheDialogComponent,
    ProjectNoActiveWritableCacheSubscriptionWarningComponent,
  ],
  templateUrl: './cache-subscriptions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class CacheSubscriptionsComponent implements OnInit {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly formatEntityLabel = formatEntityLabel;
  protected readonly links = links;
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private cachesService = inject(CachesService);
  private resolvedProject = injectProject();
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = computed(() => accessTo(this.resolvedProject(), 'manageSubscriptions'));
  private userCanCreateCache = injectCanCreateCache();
  canCreateAndSubscribe = computed(() => this.userCanCreateCache() && this.access().canEdit && !this.access().managed);
  showCreateDialog = signal(false);

  loading = signal(true);
  loadError = signal<unknown>(null);
  subscribing = signal(false);
  showSubscribeDialog = signal(false);
  errorMessage = signal<string | null>(null);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName, { label: 'Cache Subscriptions' }));
  caches = signal<CacheSubscriptionItem[]>([]);
  newCacheName = '';
  cacheSuggestions = signal<AutoCompleteSuggestion[]>([]);
  private availableCaches: Cache[] = [];

  ngOnInit(): void {
    this.projectName = this.route.snapshot.paramMap.get('project') || '';
    const preselected = this.route.snapshot.queryParamMap.get('subscribe');
    if (!preselected) {
      this.loadCaches();
      return;
    }
    void this.router.navigate([], { relativeTo: this.route, queryParams: { subscribe: null }, queryParamsHandling: 'merge', replaceUrl: true });
    this.loadCaches(() => this.openSubscribeDialog(preselected));
  }

  constructor() {
    runReloadIntervalWhileTabVisible(() => (this.loading() || this.loadError() ? EMPTY : this.fetchSubscribedCaches()));
  }

  loadCaches(onCacheSubscriptionsLoaded?: () => void): void {
    this.loading.set(true);
    this.loadError.set(null);
    this.fetchSubscribedCaches().subscribe({
      next: () => {
        this.loading.set(false);
        onCacheSubscriptionsLoaded?.();
      },
      error: (error: unknown) => {
        this.loadError.set(error);
        this.loading.set(false);
      },
    });
  }

  private fetchSubscribedCaches(): Observable<CacheSubscriptionItem[]> {
    return requestIfProjectMember(this.resolvedProject(), () => this.projectsService.getSubscribedCaches(this.projectName)).pipe(
      tap((list) => this.caches.set(list)),
    );
  }

  openSubscribeDialog(cache = ''): void {
    this.newCacheName = '';
    this.errorMessage.set(null);
    this.cacheSuggestions.set([]);
    this.showSubscribeDialog.set(true);
    this.loadAvailableCaches(cache);
  }

  // The picker shows a display name only for a value that its suggestions contain, so the preselected cache waits for them.
  private loadAvailableCaches(preselected: string): void {
    const subscribedNames = new Set(this.caches().map((c) => c.name));
    forkJoin([
      this.cachesService.getCaches(1, 100).pipe(map((own) => own.items), catchError(() => of([]))),
      this.cachesService.getPublicCaches().pipe(catchError(() => of([]))),
    ]).subscribe(([own, pub]) => {
      const seen = new Set<string>();
      this.availableCaches = [...own, ...pub].filter((c) => !subscribedNames.has(c.name) && !seen.has(c.name) && seen.add(c.name));
      if (!this.showSubscribeDialog()) return;
      this.onCacheSearch({ query: preselected || this.newCacheName });
      if (preselected) this.newCacheName = preselected;
    });
  }

  onCacheSearch(event: { query: string }): void {
    this.cacheSuggestions.set(
      this.availableCaches
        .filter((c) => doesSearchQueryMatchEntityNameOrSlug(c, event.query))
        .map((c) => entityToPickerOption(c)),
    );
  }

  createInsteadOfPick(): void {
    this.showSubscribeDialog.set(false);
    this.showCreateDialog.set(true);
  }

  createDialogClosed(visible: boolean): void {
    if (!visible) this.showSubscribeDialog.set(true);
  }

  subscribeCache(): void {
    const name = this.newCacheName.trim();
    if (!name || this.subscribing()) return;
    this.subscribing.set(true);
    this.errorMessage.set(null);
    this.projectsService.subscribeCache(this.projectName, name).subscribe({
      next: (message) => {
        this.subscribing.set(false);
        this.showSubscribeDialog.set(false);
        this.notify.success(message);
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
        target: formatEntityLabel(cache),
        consequence: pending
          ? `Delete the subscription request to ${formatEntityLabel(cache)}.`
          : `Builds for this project no longer use ${formatEntityLabel(cache)}.`,
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
