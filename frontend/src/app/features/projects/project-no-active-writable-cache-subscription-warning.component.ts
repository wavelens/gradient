/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { RouterLink } from '@angular/router';
import { MessageBannerComponent } from '@gradient/ui/ui';
import type { CacheSubscriptionItem } from '@core/models';
import { links } from '@shared/routing';

export function hasActiveCacheSubscriptionWithWriteAccess(caches: readonly CacheSubscriptionItem[]): boolean {
  return caches.some((cache) => cache.status === 'active' && cache.mode !== 'ReadOnly');
}

@Component({
  selector: 'app-project-no-active-writable-cache-subscription-warning',
  standalone: true,
  imports: [MessageBannerComponent, RouterLink],
  template: `
    @if (visible()) {
      <gr-message-banner type="warning" data-testid="no-cache-banner">
        @if (requestPending()) {
          <strong>Subscription request pending</strong>
          Evaluations wait until a cache admin approves the request.
          @if (showCacheSubscriptionsPageLink()) {
            <a [routerLink]="links.projectPage(projectName(), 'caches')">View request</a>
          }
        } @else {
          <strong>No writable cache</strong>
          Evaluations wait for the project to subscribe to a writable cache.
          @if (showCacheSubscriptionsPageLink()) {
            <a [routerLink]="links.projectPage(projectName(), 'caches')">Add a cache</a>
          }
        }
      </gr-message-banner>
    }
  `,
  styles: ':host { display: contents; }',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class ProjectNoActiveWritableCacheSubscriptionWarningComponent {
  protected readonly links = links;
  readonly projectName = input.required<string>();
  readonly caches = input.required<readonly CacheSubscriptionItem[] | null>();
  readonly showCacheSubscriptionsPageLink = input(true);
  protected readonly visible = computed(() => {
    const caches = this.caches();
    return caches !== null && !hasActiveCacheSubscriptionWithWriteAccess(caches);
  });
  protected readonly requestPending = computed(() =>
    (this.caches() ?? []).some((cache) => cache.status === 'pending' && cache.mode !== 'ReadOnly'),
  );
}
