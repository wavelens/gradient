/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { CacheSubscriptionsComponent } from './cache-subscriptions.component';
import { OrganizationsService } from '@core/services/organizations.service';
import { CachesService } from '@core/services/caches.service';
import { OrgAccessService } from '@core/services/org-access.service';
import { AccessState } from '@core/models/access.model';

function activatedRouteStub() {
  return { snapshot: { paramMap: convertToParamMap({ org: 'acme' }) } } as Partial<ActivatedRoute>;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(access: AccessState): ComponentFixture<CacheSubscriptionsComponent> {
  TestBed.configureTestingModule({
    imports: [CacheSubscriptionsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub() },
      {
        provide: OrganizationsService,
        useValue: {
          getOrganization: () => of({ id: 'o', display_name: 'Acme' }),
          getSubscribedCaches: () => of([{ id: 'c', name: 'shared' }]),
        },
      },
      { provide: CachesService, useValue: {} },
      { provide: OrgAccessService, useValue: { forOrg: () => Promise.resolve(access) } },
    ],
  });
  return TestBed.createComponent(CacheSubscriptionsComponent);
}

async function settled(fixture: ComponentFixture<CacheSubscriptionsComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('CacheSubscriptionsComponent — access gating', () => {
  it('hides Subscribe and Unsubscribe under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'subscribe to cache')).toBeNull();
    expect(findByText(fixture.nativeElement, 'unsubscribe')).toBeNull();
  });

  it('shows but disables Subscribe under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const btn = findByText(fixture.nativeElement, 'subscribe to cache') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.disabled).toBe(true);
  });
});
