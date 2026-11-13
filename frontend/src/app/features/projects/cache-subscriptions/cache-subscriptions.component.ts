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
import { injectProject } from '@core/resolvers/project-access.resolver';
import { NotifyService } from '@core/services/notify.service';
import { apiErrorMessage } from '@core/services/api.service';
import {
  AutoCompleteComponent,
  BadgeComponent,
  ButtonComponent,
  CheckboxComponent,
  DialogComponent,
  EmptyStateComponent,
  FormFieldComponent,
  LoadStateComponent,
  PageLayoutComponent,
  RowComponent,
  RowListComponent,
  SelectComponent,
  TooltipDirective,
  type AutoCompleteSuggestion,
} from '@gradient/ui/ui';
import { EntityNameComponent, LabelHelpComponent, doesSearchQueryMatchEntityNameOrSlug, entityKindToIconName, entityToPickerOption, formatEntityLabel } from '@shared/ui';
import {
  WritableDirective,
  ManagedDisableDirective,
  ServerConfigurationManagedLockButtonComponent,
  SettingsAccessStateBadgeComponent,
  requestIfProjectMember,
} from '@shared/access';
import { ConfirmService } from '@shared/confirm';
import { CreateCacheDialogComponent, injectCanCreateCache } from '@shared/forms';
import { SUBSCRIPTION_MODES, subscriptionModeLabel } from '@shared/text';
import { accessTo, CacheSubscriptionItem, CacheSubscriptionMode, SubscribableCache } from '@core/models';
import { links } from '@shared/routing';
import { runReloadIntervalWhileTabVisible } from '@shared/paging';
import { EMPTY, Observable, tap } from 'rxjs';
import { ProjectSettingsNavigationComponent } from '../project-settings-navigation/project-settings-navigation.component';

const subscriptionRequestedResponseMessage = 'Subscription requested';

@Component({
  selector: 'app-cache-subscriptions',
  standalone: true,
  imports: [
    ProjectSettingsNavigationComponent,
    EntityNameComponent,
    LabelHelpComponent,
    CommonModule,
    RouterModule,
    FormsModule,
    DialogComponent,
    BadgeComponent,
    ButtonComponent,
    CheckboxComponent,
    SelectComponent,
    AutoCompleteComponent,
    LoadStateComponent,
    WritableDirective,
    ManagedDisableDirective,
    ServerConfigurationManagedLockButtonComponent,
    SettingsAccessStateBadgeComponent,
    PageLayoutComponent,
    FormFieldComponent,
    EmptyStateComponent,
    RowListComponent,
    RowComponent,
    TooltipDirective,
    CreateCacheDialogComponent,
  ],
  templateUrl: './cache-subscriptions.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class CacheSubscriptionsComponent implements OnInit {
  protected readonly entityKindToIconName = entityKindToIconName;
  protected readonly formatEntityLabel = formatEntityLabel;
  protected readonly links = links;
  protected readonly modes = SUBSCRIPTION_MODES;
  protected readonly modeLabel = subscriptionModeLabel;
  private route = inject(ActivatedRoute);
  private router = inject(Router);
  private projectsService = inject(ProjectsService);
  private resolvedProject = injectProject();
  private crumbs = inject(BreadcrumbsService);
  private notify = inject(NotifyService);
  private confirm = inject(ConfirmService);

  access = computed(() => accessTo(this.resolvedProject(), 'manageSubscriptions'));
  private userCanCreateCache = injectCanCreateCache();
  canCreateAndSubscribe = computed(() => this.userCanCreateCache() && this.access().canEdit && !this.access().managed);
  showCreateDialog = signal(false);

  protected shouldShowLockIconForSubscription(cache: CacheSubscriptionItem): boolean {
    return !!cache.managed && !this.access().managed;
  }

  loading = signal(true);
  loadError = signal<unknown>(null);
  subscribing = signal(false);
  showSubscribeDialog = signal(false);
  errorMessage = signal<string | null>(null);

  projectName = '';
  breadcrumb = computed(() => this.crumbs.projectSettings(this.projectName));
  caches = signal<CacheSubscriptionItem[]>([]);
  newCacheName = '';
  selectedSubscriptionMode = signal<CacheSubscriptionMode>('ReadWrite');
  signPushedPathsPendingValue = signal<{ cache: string; value: boolean } | null>(null);
  cacheSuggestions = signal<AutoCompleteSuggestion[]>([]);
  private availableCaches: SubscribableCache[] = [];

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
    this.selectedSubscriptionMode.set('ReadWrite');
    this.errorMessage.set(null);
    this.cacheSuggestions.set([]);
    this.showSubscribeDialog.set(true);
    this.loadAvailableCaches(cache);
  }

  // The picker shows a display name only for a value that its suggestions contain, so the preselected cache waits for them.
  private loadAvailableCaches(preselected: string): void {
    this.projectsService.getSubscribableCaches(this.projectName).subscribe({
      next: (caches) => {
        this.availableCaches = caches;
        if (!this.showSubscribeDialog()) return;
        this.onCacheSearch({ query: preselected || this.newCacheName });
        if (preselected) this.newCacheName = preselected;
      },
      error: (error: unknown) => {
        if (preselected) this.newCacheName = preselected;
        this.errorMessage.set(apiErrorMessage(error, "Couldn't load the caches that this project can subscribe to."));
      },
    });
  }

  onCacheSearch(event: { query: string }): void {
    this.cacheSuggestions.set(
      this.availableCaches
        .filter((c) => doesSearchQueryMatchEntityNameOrSlug(c, event.query))
        .map((c) => ({ ...entityToPickerOption(c), textBelowLabel: this.buildSubscribableCacheTextBelowLabel(c) })),
    );
  }

  private buildSubscribableCacheTextBelowLabel(cache: SubscribableCache): string {
    const approval = !cache.needs_approval
      ? 'Write access'
      : cache.approvers.length > 0
        ? `Pending approval from ${cache.approvers.join(', ')}`
        : 'Ask a cache administrator for approval.';
    return [cache.owner_team_display_name ?? cache.owner_team, cache.public ? 'Public' : 'Private', approval].filter(Boolean).join(' · ');
  }

  buildPendingSubscriptionApproversText(cache: CacheSubscriptionItem): string {
    return cache.approvers.length > 0 ? `Cache admins: ${cache.approvers.join(', ')}` : '';
  }

  getSubscriptionSignPushedPathsCheckboxValue(cache: CacheSubscriptionItem): boolean {
    const pending = this.signPushedPathsPendingValue();
    return pending?.cache === cache.name ? pending.value : cache.sign_outputs;
  }

  toggleSubscriptionSignPushedPaths(cache: CacheSubscriptionItem, value: boolean): void {
    if (this.signPushedPathsPendingValue()) return;
    this.signPushedPathsPendingValue.set({ cache: cache.name, value });
    this.projectsService.setCacheSubscriptionSignOutputs(this.projectName, cache.name, value).subscribe({
      next: () => {
        this.caches.update((list) => list.map((c) => (c.name === cache.name ? { ...c, sign_outputs: value } : c)));
        this.signPushedPathsPendingValue.set(null);
        this.notify.success('Cache subscription saved');
      },
      error: (error: unknown) => {
        this.signPushedPathsPendingValue.set(null);
        this.notify.failure("Couldn't save cache subscription", error);
      },
    });
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
    const approvers = this.availableCaches.find((c) => c.name === name)?.approvers ?? [];
    this.projectsService.subscribeCache(this.projectName, name, this.selectedSubscriptionMode()).subscribe({
      next: (message) => {
        this.subscribing.set(false);
        this.showSubscribeDialog.set(false);
        if (message !== subscriptionRequestedResponseMessage) this.notify.success(message);
        else if (approvers.length > 0) this.notify.success(`Subscription requested. Approval from a cache admin is pending: ${approvers.join(', ')}`);
        else this.notify.success('Subscription requested. Pending approval from a cache administrator.');
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
