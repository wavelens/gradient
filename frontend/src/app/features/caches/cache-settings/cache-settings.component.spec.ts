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
import { CacheSettingsComponent } from './cache-settings.component';
import { CachesService } from '@core/services/caches.service';
import { AccessState } from '@core/models/access.model';

function cacheFor(access: AccessState) {
  return {
    id: 'c',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    active: true,
    priority: 50,
    max_storage_gb: 0,
    public: false,
    managed: access.managed,
    can_edit: access.canEdit,
  };
}

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'demo' }) },
    data: of({}),
    parent: { data: of({ cacheAccess: { cache: cacheFor(access), access } }) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(access: AccessState): ComponentFixture<CacheSettingsComponent> {
  TestBed.configureTestingModule({
    imports: [CacheSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      { provide: CachesService, useValue: { getCache: () => of(cacheFor(access)) } },
    ],
  });
  const fixture = TestBed.createComponent(CacheSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('CacheSettingsComponent - access gating', () => {
  it('hides Save / Delete / Toggle under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete cache')).toBeNull();
    expect(findByText(fixture.nativeElement, 'deactivate')).toBeNull();
  });

  it('shows but disables Save / Delete under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    const del = findByText(fixture.nativeElement, 'delete cache') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(true);
    expect(del).not.toBeNull();
    expect(del!.disabled).toBe(true);
  });

  it('always shows Manage Upstreams link, even when state-managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const link = (Array.from(fixture.nativeElement.querySelectorAll('a')) as HTMLAnchorElement[]).find(
      (el) => (el.textContent ?? '').toLowerCase().includes('manage upstreams'),
    );
    expect(link).toBeDefined();
  });
});
