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
import { CacheUpstreamsComponent } from './cache-upstreams.component';
import { CachesService } from '@core/services/caches.service';
import { AccessState } from '@core/models/access.model';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'demo' }) },
    data: of({}),
    parent: { data: of({ cacheAccess: { cache: {}, access } }) },
  } as unknown as ActivatedRoute;
}

const oneUpstream = [
  {
    id: 'u1',
    display_name: 'Upstream A',
    mode: 'ReadOnly' as const,
    upstream_cache_id: 'cache-1',
    url: null,
    public_key: null,
  },
];

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function findIconButton(root: HTMLElement, icon: string): HTMLButtonElement | null {
  return (
    (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).find(
      (el) => !!el.querySelector(`.${icon}`),
    ) ?? null
  );
}

function setup(access: AccessState): ComponentFixture<CacheUpstreamsComponent> {
  TestBed.configureTestingModule({
    imports: [CacheUpstreamsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      {
        provide: CachesService,
        useValue: {
          getCache: () => of({ display_name: 'Demo' }),
          getCacheUpstreams: () => of(oneUpstream),
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(CacheUpstreamsComponent);
  fixture.detectChanges();
  return fixture;
}

describe('CacheUpstreamsComponent - HTTP upstream probe', () => {
  const realFetch = globalThis.fetch;
  afterEach(() => { globalThis.fetch = realFetch; });

  function probeWith(url: string, fetchImpl: typeof fetch) {
    globalThis.fetch = fetchImpl;
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    fixture.componentInstance.upstreamForm.url = url;
    return fixture.componentInstance;
  }

  it('does not probe a scheme-less URL that would hit our own origin', async () => {
    const fetchSpy = vi.fn();
    const component = probeWith('randomtext', fetchSpy as unknown as typeof fetch);
    await component.probeHttpUrl();
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(component.probeSuggestsProto()).toBe(false);
  });

  it('suggests proto only when the body is a real gradient-cache-info', async () => {
    const fetchSpy = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.resolve({ GradientVersion: '0.1.0', GradientUrl: 'https://g.example.com' }),
    });
    const component = probeWith('https://g.example.com', fetchSpy as unknown as typeof fetch);
    await component.probeHttpUrl();
    expect(fetchSpy).toHaveBeenCalledWith('https://g.example.com/gradient-cache-info?json', expect.anything());
    expect(component.probeSuggestsProto()).toBe(true);
  });

  it('does not suggest proto when a 200 returns a non-gradient body', async () => {
    const fetchSpy = vi.fn().mockResolvedValue({
      ok: true,
      json: () => Promise.reject(new SyntaxError('Unexpected token <')),
    });
    const component = probeWith('https://not-gradient.example.com', fetchSpy as unknown as typeof fetch);
    await component.probeHttpUrl();
    expect(component.probeSuggestsProto()).toBe(false);
  });
});

describe('CacheUpstreamsComponent - access gating', () => {
  it('renders the upstream list under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(fixture.nativeElement.textContent).toContain('Upstream A');
  });

  it('hides Add Upstream, Edit, Delete under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'add upstream')).toBeNull();
    expect(findIconButton(fixture.nativeElement, 'pi-pencil')).toBeNull();
    expect(findIconButton(fixture.nativeElement, 'pi-trash')).toBeNull();
  });

  it('shows but disables Add / Edit / Delete under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const addBtn = findByText(fixture.nativeElement, 'add upstream') as HTMLButtonElement | null;
    const editBtn = findIconButton(fixture.nativeElement, 'pi-pencil');
    const delBtn = findIconButton(fixture.nativeElement, 'pi-trash');
    expect(addBtn).not.toBeNull();
    expect(addBtn!.disabled).toBe(true);
    expect(editBtn).not.toBeNull();
    expect(editBtn!.disabled).toBe(true);
    expect(delBtn).not.toBeNull();
    expect(delBtn!.disabled).toBe(true);
  });

  it('renders Add / Edit / Delete enabled under full access', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const addBtn = findByText(fixture.nativeElement, 'add upstream') as HTMLButtonElement | null;
    expect(addBtn).not.toBeNull();
    expect(addBtn!.disabled).toBe(false);
  });
});
