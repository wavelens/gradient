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
  it('shows the mode for every upstream kind', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [...oneUpstream, httpUpstream]);
    const rows = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-row'));
    const badges = rows.map((row) => Array.from(row.querySelectorAll('gr-badge')).map((b) => b.textContent?.trim()));
    expect(badges).toEqual([['Internal', 'Read Only'], ['HTTP', 'Read Only']]);
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
    expect((fixture.nativeElement as HTMLElement).querySelector('gr-message-banner')?.textContent).toContain('come from the server configuration');
    expect(addBtn).not.toBeNull();
    expect(addBtn!.getAttribute('aria-disabled')).toBe('true');
    expect(editBtn).not.toBeNull();
    expect(editBtn!.getAttribute('aria-disabled')).toBe('true');
    expect(delBtn).not.toBeNull();
    expect(delBtn!.getAttribute('aria-disabled')).toBe('true');
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

  it('clears the failed attempt when the type changes', () => {
    const fixture = setup(admin);
    const component = fixture.componentInstance;
    component.openAddDialog();
    component.switchUpstreamType('http');
    component.addUpstream();
    component.addError.set('Failed to add upstream cache.');
    fixture.detectChanges();
    expect(fieldText('up-url')).toContain('Enter the cache URL.');
    component.switchUpstreamType('gradient_proto');
    component.switchUpstreamType('http');
    fixture.detectChanges();
    expect(component.addError()).toBeNull();
    expect(fieldText('up-url')).not.toContain('Enter the cache URL.');
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

describe('UpstreamCachesComponent - connection test', () => {
  const probe = (ok: boolean) => ({ ok, status: ok ? 200 : null, latency_ms: 5, error: ok ? null : 'protocol error' });

  it.each([
    [false, true],
    [true, false],
  ])('shows the result in the tested row and suggests HTTP/1.1 only when only HTTP/1.1 works (already on: %s)', (http1Only, hinted) => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [{ ...httpUpstream, http1_only: http1Only }]);
    Object.assign(TestBed.inject(CachesService), { testUpstream: () => of({ ok: true, message: 'Reachable over HTTP/1.1 only.', http1: probe(true), http2: probe(false) }) });
    fixture.componentInstance.testUpstream({ ...httpUpstream, http1_only: http1Only });
    fixture.detectChanges();
    const result = (fixture.nativeElement as HTMLElement).querySelector('gr-row .connection-test-result')!;
    expect(result.textContent).toContain('Reachable over HTTP/1.1 only.');
    expect(result.classList).not.toContain('failed');
    expect(result.textContent?.includes('Turn on "HTTP/1.1 only" in Edit.')).toBe(hinted);
    expect(TestBed.inject(MessageService).messages()).toEqual([]);
  });

  it('marks an unreachable upstream as failed in its row', () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, [httpUpstream]);
    Object.assign(TestBed.inject(CachesService), { testUpstream: () => of({ ok: false, message: 'Unreachable over HTTP/1.1 and HTTP/2.', http1: probe(false), http2: probe(false) }) });
    fixture.componentInstance.testUpstream(httpUpstream);
    fixture.detectChanges();
    const result = (fixture.nativeElement as HTMLElement).querySelector('gr-row .connection-test-result')!;
    expect(result.classList).toContain('failed');
    expect(result.textContent).toContain('HTTP/1.1: failed - protocol error');
  });
});

describe('UpstreamCachesComponent - removal', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('asks before removing and names the upstream cache', () => {
    const removeUpstream = vi.fn(() => of('ok'));
    const confirm = vi.fn((request: ConfirmRequest) => request.action());
    const fixture = setup(admin, oneUpstream, undefined, { removeUpstream, confirm });
    findIconButton(fixture.nativeElement, 'delete')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete upstream cache', target: 'Upstream A', confirmLabel: 'Delete' }));
    expect(removeUpstream).toHaveBeenCalledWith('demo', 'u1');
    expect(TestBed.inject(MessageService).messages()).toContainEqual(
      expect.objectContaining({ severity: 'success', summary: 'Upstream cache deleted' }),
    );
  });

  it('sends nothing when the removal is cancelled', () => {
    const removeUpstream = vi.fn(() => of('ok'));
    const fixture = setup(admin, oneUpstream, undefined, { removeUpstream, confirm: vi.fn(() => EMPTY) });
    findIconButton(fixture.nativeElement, 'delete')!.click();
    expect(removeUpstream).not.toHaveBeenCalled();
  });
});
