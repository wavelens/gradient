/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { cacheAccessData } from '@core/resolvers/access-route.testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, NEVER, Observable, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { UpstreamCachesComponent, probeSummary } from './upstream-caches.component';
import { CachesService } from '@core/services/caches.service';
import { AccessState } from '@core/models/access.model';
import { MessageService } from '@gradient/ui/ui';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'demo' }) },
    data: of({}),
    parent: { data: of(cacheAccessData(access)) },
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
  upstreamCaches: unknown[] | (() => Observable<unknown[]>) = oneUpstream,
  updateUpstream: (...args: unknown[]) => unknown = () => of('ok'),
  extra: { removeUpstream?: unknown; confirm?: unknown } = {},
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
          getUpstreamCaches: Array.isArray(upstreamCaches) ? () => of(upstreamCaches) : upstreamCaches,
          updateUpstream,
          removeUpstream: extra.removeUpstream ?? vi.fn(() => of('ok')),
        },
      },
      ...(extra.confirm ? [{ provide: ConfirmService, useValue: { confirm: extra.confirm } }] : []),
    ],
  });
  const fixture = TestBed.createComponent(UpstreamCachesComponent);
  fixture.detectChanges();
  return fixture;
}

describe('UpstreamCachesComponent - badges', () => {
  it('show the mode only for internal upstream caches, since external upstream caches are always read-only', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [...oneUpstream, httpUpstream]);
    const rows = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-row'));
    const badges = rows.map((row) => Array.from(row.querySelectorAll('gr-badge')).map((b) => b.textContent?.trim()));
    expect(badges).toEqual([['Internal', 'Read Only'], ['HTTP']]);
  });
});

describe('UpstreamCachesComponent - loading', () => {
  it('shows a failed load with Try Again instead of No upstream caches', () => {
    const load = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
      .mockReturnValue(of(oneUpstream));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, load);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load upstream caches');
    expect(root.textContent).not.toContain('No upstream caches');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(load).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('Upstream A');
  });
});

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

  it('shows but disables Add / Edit / Delete under state-managed access and says why', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const addBtn = findByText(fixture.nativeElement, 'add upstream cache') as HTMLButtonElement | null;
    const editBtn = findIconButton(fixture.nativeElement, 'edit');
    const delBtn = findIconButton(fixture.nativeElement, 'delete');
    expect((fixture.nativeElement as HTMLElement).querySelector('gr-message-banner')?.textContent).toContain('managed by the server configuration');
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

describe('UpstreamCachesComponent - HTTP/1.1 only', () => {
  it('saves HTTP/1.1 only from the edit form of an HTTP upstream cache', () => {
    const updateUpstream = vi.fn().mockReturnValue(of('ok'));
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [httpUpstream], updateUpstream);
    const component = fixture.componentInstance;
    component.openEditDialog(httpUpstream);
    component.editForm.http1_only = true;
    component.saveUpstream();
    expect(updateUpstream).toHaveBeenCalledWith('demo', 'u2', expect.objectContaining({ http1_only: true }));
  });
});

describe('UpstreamCachesComponent - field checks', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };
  const protoUpstream = {
    ...httpUpstream,
    id: 'u3',
    display_name: 'Remote',
    mode: 'ReadWrite' as const,
    kind: 'gradientproto',
    url: 'https://gradient.example.com',
    public_key: null,
    remote_cache: 'main',
  };
  const fieldText = (id: string) => document.querySelector(`#${id}`)!.closest('gr-form-field')!.textContent ?? '';

  it('edits the mode of a Gradient Proto upstream instead of a public key', () => {
    const updateUpstream = vi.fn().mockReturnValue(of('ok'));
    const fixture = setup(admin, [protoUpstream], updateUpstream);
    const component = fixture.componentInstance;
    expect(fixture.nativeElement.textContent).toContain('Gradient Proto');
    component.openEditDialog(protoUpstream);
    fixture.detectChanges();
    expect(document.querySelector('#edit-pubkey')).toBeNull();
    expect(document.querySelector('#edit-mode')).not.toBeNull();
    component.editForm.mode = 'ReadOnly';
    component.saveUpstream();
    expect(updateUpstream).toHaveBeenCalledWith('demo', 'u3', { display_name: 'Remote', mode: 'ReadOnly' });
  });

  it('shows an emptied URL under its field and sends nothing', () => {
    const updateUpstream = vi.fn().mockReturnValue(of('ok'));
    const fixture = setup(admin, [httpUpstream], updateUpstream);
    const component = fixture.componentInstance;
    component.openEditDialog(httpUpstream);
    component.editForm.url = '';
    fixture.detectChanges();
    findByText(document.querySelector('.gr-dialog__footer')!, 'save')!.click();
    fixture.detectChanges();
    expect(fieldText('edit-url')).toContain('Enter the cache URL.');
    expect(updateUpstream).not.toHaveBeenCalled();
  });

  it('refuses a public key without a name before asking the server', () => {
    const updateUpstream = vi.fn().mockReturnValue(of('ok'));
    const fixture = setup(admin, [httpUpstream], updateUpstream);
    const component = fixture.componentInstance;
    component.openEditDialog(httpUpstream);
    component.editForm.public_key = '6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=';
    fixture.detectChanges();
    expect(fieldText('edit-pubkey')).toContain('name:key');
    document.querySelector('#edit-pubkey')!.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    expect(updateUpstream).not.toHaveBeenCalled();
  });

  it('starts a new upstream as Read Only and switches the type with the shared tab switch', () => {
    const fixture = setup(admin);
    const component = fixture.componentInstance;
    component.openAddDialog();
    fixture.detectChanges();
    expect(component.upstreamForm.mode).toBe('ReadOnly');
    const tabs = Array.from(document.querySelectorAll<HTMLButtonElement>('gr-tab-switch button'));
    tabs.find((tab) => tab.textContent?.includes('HTTP'))!.click();
    fixture.detectChanges();
    expect(component.upstreamType).toBe('http');
  });

  it('names every missing field of a new HTTP upstream and adds nothing', () => {
    const addHttpUpstream = vi.fn(() => of('ok'));
    const fixture = setup(admin);
    Object.assign(TestBed.inject(CachesService), { addHttpUpstream });
    const component = fixture.componentInstance;
    component.openAddDialog();
    component.upstreamType = 'http';
    fixture.detectChanges();
    findByText(document.querySelector('.gr-dialog__footer')!, 'add')!.click();
    fixture.detectChanges();
    expect(fieldText('up-display-name')).toContain('Enter a display name.');
    expect(fieldText('up-url')).toContain('Enter the cache URL.');
    expect(fieldText('up-pubkey')).toContain('Enter the public key.');
    expect(addHttpUpstream).not.toHaveBeenCalled();
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

describe('UpstreamCachesComponent - write failures', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('shows a failed deactivate as an error toast with the server reason', () => {
    const fixture = setup(admin, oneUpstream, () => throwError(() => new ApiError('Upstream is locked', 409)));
    (findByText(fixture.nativeElement, 'deactivate') as HTMLButtonElement).click();
    expect(TestBed.inject(MessageService).messages()).toContainEqual(
      expect.objectContaining({ severity: 'error', summary: 'Upstream cache deactivation failed', detail: 'Upstream is locked' }),
    );
    expect(fixture.componentInstance.togglingUpstreamId()).toBeNull();
  });

  it('keeps a failed save inside the open Edit dialog', () => {
    const fixture = setup(admin, [httpUpstream], () => throwError(() => new ApiError('Public key is malformed', 400)));
    const component = fixture.componentInstance;
    component.openEditDialog(httpUpstream);
    fixture.detectChanges();
    component.saveUpstream();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Public key is malformed');
    expect(component.showEditDialog()).toBe(true);
    expect(component.savingUpstream()).toBe(false);
  });

  it('keeps the Edit dialog open while the save runs', () => {
    const updateUpstream = vi.fn(() => NEVER);
    const fixture = setup(admin, [httpUpstream], updateUpstream);
    const component = fixture.componentInstance;
    component.openEditDialog(httpUpstream);
    fixture.detectChanges();
    const dialog = document.querySelector('.gr-dialog') as HTMLElement;
    findByText(dialog, 'save')!.click();
    fixture.detectChanges();
    findByText(dialog, 'save')!.click();
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(updateUpstream).toHaveBeenCalledTimes(1);
    expect(component.showEditDialog()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });
});

describe('UpstreamCachesComponent - removal', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('asks before removing and names the upstream cache', () => {
    const removeUpstream = vi.fn(() => of('ok'));
    const confirm = vi.fn((request: ConfirmRequest) => request.action());
    const fixture = setup(admin, oneUpstream, undefined, { removeUpstream, confirm });
    findIconButton(fixture.nativeElement, 'delete')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Upstream A' }));
    expect(removeUpstream).toHaveBeenCalledWith('demo', 'u1');
    expect(TestBed.inject(MessageService).messages()).toContainEqual(
      expect.objectContaining({ severity: 'success', summary: 'Upstream cache removed' }),
    );
  });

  it('sends nothing when the removal is cancelled', () => {
    const removeUpstream = vi.fn(() => of('ok'));
    const fixture = setup(admin, oneUpstream, undefined, { removeUpstream, confirm: vi.fn(() => EMPTY) });
    findIconButton(fixture.nativeElement, 'delete')!.click();
    expect(removeUpstream).not.toHaveBeenCalled();
  });
});
