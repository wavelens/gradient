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
import { UpstreamCachesComponent, probeSummary } from './upstream-caches.component';
import { CachesService } from '@core/services/caches.service';
import { AccessState } from '@core/models/access.model';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'demo' }) },
    data: of({}),
    parent: { data: of({ cacheAccess: { cache: {}, access } }) },
  } as unknown as ActivatedRoute;
}

const httpUpstream = {
  id: 'u2',
  display_name: 'Upstream B',
  mode: 'ReadOnly' as const,
  upstream_cache_id: null,
  kind: 'http' as const,
  url: 'https://cache.example.org',
  public_key: 'cache.example.org-1:abc',
  http1_only: false,
  active: true,
};

const oneUpstream = [
  {
    id: 'u1',
    display_name: 'Upstream A',
    mode: 'ReadOnly' as const,
    upstream_cache_id: 'cache-1',
    kind: 'internal' as const,
    url: null,
    public_key: null,
    http1_only: false,
    active: true,
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
      (el) => el.querySelector('.gr-button__icon')?.textContent?.trim() === icon,
    ) ?? null
  );
}

function setup(
  access: AccessState,
  upstreamCaches: unknown[] = oneUpstream,
  updateUpstream: (...args: unknown[]) => unknown = () => of('ok'),
): ComponentFixture<UpstreamCachesComponent> {
  TestBed.configureTestingModule({
    imports: [UpstreamCachesComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      {
        provide: CachesService,
        useValue: {
          getCache: () => of({ display_name: 'Demo' }),
          getUpstreamCaches: () => of(upstreamCaches),
          updateUpstream,
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(UpstreamCachesComponent);
  fixture.detectChanges();
  return fixture;
}

describe('UpstreamCachesComponent - HTTP upstream probe', () => {
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

describe('UpstreamCachesComponent - access gating', () => {
  it('renders the upstream cache list under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(fixture.nativeElement.textContent).toContain('Upstream A');
  });

  it('hides Add Upstream Cache, Edit, Delete under read-only access', () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    expect(findByText(fixture.nativeElement, 'add upstream cache')).toBeNull();
    expect(findIconButton(fixture.nativeElement, 'edit')).toBeNull();
    expect(findIconButton(fixture.nativeElement, 'delete')).toBeNull();
  });

  it('shows but disables Add / Edit / Delete under state-managed access', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const addBtn = findByText(fixture.nativeElement, 'add upstream cache') as HTMLButtonElement | null;
    const editBtn = findIconButton(fixture.nativeElement, 'edit');
    const delBtn = findIconButton(fixture.nativeElement, 'delete');
    expect(addBtn).not.toBeNull();
    expect(addBtn!.disabled).toBe(true);
    expect(editBtn).not.toBeNull();
    expect(editBtn!.disabled).toBe(true);
    expect(delBtn).not.toBeNull();
    expect(delBtn!.disabled).toBe(true);
  });

  it('renders Add / Edit / Delete enabled under full access', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const addBtn = findByText(fixture.nativeElement, 'add upstream cache') as HTMLButtonElement | null;
    expect(addBtn).not.toBeNull();
    expect(addBtn!.disabled).toBe(false);
  });
});

describe('UpstreamCachesComponent - protocol test', () => {
  it('offers Test only for HTTP binary-cache upstreams', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    expect(findByText(fixture.nativeElement, 'test')).toBeNull();
  });

  it('keeps Test usable on a state-managed cache, since a test changes nothing', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true }, [httpUpstream]);
    const testBtn = findByText(fixture.nativeElement, 'test') as HTMLButtonElement | null;
    expect(testBtn).not.toBeNull();
    expect(testBtn!.disabled).toBe(false);
  });

  it('summarises a failed protocol by its error, else its status', () => {
    expect(probeSummary({ ok: true, status: 200, latency_ms: 12, error: null })).toBe('ok (12 ms)');
    expect(probeSummary({ ok: false, status: null, latency_ms: 3, error: 'connection reset' }))
      .toBe('failed - connection reset');
    expect(probeSummary({ ok: false, status: 404, latency_ms: 3, error: null })).toBe('failed - status 404');
  });
});

describe('UpstreamCachesComponent - activation', () => {
  it('deactivates an active upstream cache without touching its other settings', () => {
    const updateUpstream = vi.fn().mockReturnValue(of('ok'));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, oneUpstream, updateUpstream);
    (findByText(fixture.nativeElement, 'deactivate') as HTMLButtonElement).click();
    expect(updateUpstream).toHaveBeenCalledWith('demo', 'u1', { active: false });
  });

  it('keeps Deactivate usable on a state-managed cache, since state restores it on restart', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const toggle = findByText(fixture.nativeElement, 'deactivate') as HTMLButtonElement | null;
    expect(toggle).not.toBeNull();
    expect(toggle!.disabled).toBe(false);
  });

  it('marks an inactive upstream cache and offers Activate', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [{ ...httpUpstream, active: false }]);
    expect(fixture.nativeElement.textContent).toContain('Inactive');
    expect(findByText(fixture.nativeElement, 'deactivate')).toBeNull();
    expect(findByText(fixture.nativeElement, 'activate')).not.toBeNull();
  });
});
