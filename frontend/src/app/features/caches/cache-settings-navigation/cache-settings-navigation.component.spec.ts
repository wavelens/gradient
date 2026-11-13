/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { cacheAccessData } from '@core/resolvers/access-route.testing';
import { AuthService } from '@core/services/auth.service';
import { CachesService } from '@core/services/caches.service';
import { BinaryCacheSettingsNavigationComponent } from './cache-settings-navigation.component';

const getSubscriptionRequests = vi.fn(() => of([{ project: 'p1' }, { project: 'p2' }]));

async function render(canEdit: boolean, livePendingCount?: number, signedInPersonRoleAndSuperuser: { role?: string | null; superuser?: boolean } = {}) {
  const { superuser = false, ...role } = signedInPersonRoleAndSuperuser;
  getSubscriptionRequests.mockClear();
  HTMLElement.prototype.scrollIntoView ??= () => {};
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      { provide: CachesService, useValue: { getSubscriptionRequests } },
      { provide: AuthService, useValue: { user: signal({ superuser }) } },
      { provide: ActivatedRoute, useValue: { parent: { data: of(cacheAccessData({ managed: false, canEdit, canTrigger: canEdit }, { name: 'main', ...role })) } } },
    ],
  });
  const fixture = TestBed.createComponent(BinaryCacheSettingsNavigationComponent);
  fixture.componentRef.setInput('cacheUrlName', 'main');
  if (livePendingCount !== undefined) fixture.componentRef.setInput('pendingSubscriptionRequestCount', livePendingCount);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

const labels = (root: HTMLElement) => Array.from(root.querySelectorAll('.side-navigation-label')).map((label) => label.textContent);

describe('BinaryCacheSettingsNavigationComponent', () => {
  it('links subscriptions with the pending count and event notifications for a cache admin', async () => {
    const root = await render(true);
    expect(labels(root)).toEqual(['General', 'Members', 'Upstream caches', 'Subscriptions', 'Event notifications']);
    expect(root.querySelector('gr-badge')?.textContent?.trim()).toBe('2 Pending');
  });

  it('Use the subscription page count without a server request and hide the badge when the count is 0.', async () => {
    const root = await render(true, 0);
    expect(root.querySelector('gr-badge')).toBeNull();
    expect(getSubscriptionRequests).not.toHaveBeenCalled();
  });

  it('The viewer shows only links to General, Members and Upstream caches.', async () => {
    expect(labels(await render(false))).toEqual(['General', 'Members', 'Upstream caches']);
  });

  it.each([
    { role: 'View', superuser: false, shown: true },
    { role: null, superuser: true, shown: true },
    { role: null, superuser: false, shown: false },
  ])('Cache members and instance administrators see a Members link. People outside the cache see no link. (%o)', async ({ role, superuser, shown }) => {
    const root = await render(false, undefined, { role, superuser });
    expect(root.querySelector('a[href="/caches/main/members"]') !== null).toBe(shown);
  });
});
