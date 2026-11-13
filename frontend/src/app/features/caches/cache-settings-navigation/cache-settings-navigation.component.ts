/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, inject, input } from '@angular/core';
import { SettingsPageSideNavigationComponent, type VerticalNavigationLink } from '@gradient/ui/ui';
import { injectCacheAccessData, injectCacheAccessTo } from '@core/resolvers/inject-access';
import { AuthService } from '@core/services/auth.service';
import { canOpenCacheMembersPage } from '@shared/access';
import { links } from '@shared/routing';
import { injectPendingSubscriptionRequestCount } from '../pending-subscription-request-count';

@Component({
  selector: 'app-cache-settings-navigation',
  standalone: true,
  imports: [SettingsPageSideNavigationComponent],
  template: `<gr-settings-page-side-navigation ariaLabel="Cache Settings" [links]="navigationLinkItems()" />`,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class BinaryCacheSettingsNavigationComponent {
  cacheUrlName = input.required<string>();
  pendingSubscriptionRequestCount = input<number>();

  private authService = inject(AuthService);
  private cacheAccess = injectCacheAccessData();
  private manageCacheSubscriptionsAccess = injectCacheAccessTo('manageCacheSubscriptions');
  private webhooksAccess = injectCacheAccessTo('manageCacheWebhooks');
  private fetchedPendingSubscriptionRequestCount = injectPendingSubscriptionRequestCount(() => this.pendingSubscriptionRequestCount() === undefined);

  protected navigationLinkItems = computed<VerticalNavigationLink[]>(() => {
    const cache = this.cacheUrlName();
    const pending = this.pendingSubscriptionRequestCount() === undefined ? this.fetchedPendingSubscriptionRequestCount() : this.pendingSubscriptionRequestCount();
    return [
      { label: 'General', icon: 'tune', link: links.cachePage(cache, 'settings') },
      ...(canOpenCacheMembersPage(this.cacheAccess()?.cache, this.authService.user())
        ? [{ label: 'Members', icon: 'group', link: links.cachePage(cache, 'members') }]
        : []),
      { label: 'Upstream caches', icon: 'cloud_download', link: links.cachePage(cache, 'upstream-caches') },
      ...(this.manageCacheSubscriptionsAccess().canEdit
        ? [{ label: 'Subscriptions', icon: 'hub', link: links.cachePage(cache, 'subscriptions'), badge: pending ? `${pending} Pending` : undefined }]
        : []),
      ...(this.webhooksAccess().canEdit
        ? [{ label: 'Event notifications', icon: 'webhook', link: links.cachePage(cache, 'webhooks') }]
        : []),
    ];
  });
}
