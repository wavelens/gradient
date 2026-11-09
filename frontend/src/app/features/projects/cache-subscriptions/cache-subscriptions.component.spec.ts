/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Observable, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { CacheSubscriptionsComponent } from './cache-subscriptions.component';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { projectRouteStub } from '@core/resolvers/access-route.testing';
import { ConfigService } from '@core/services/config.service';
import { AuthService } from '@core/services/auth.service';
import { AccessState } from '@core/models/access.model';
import { By } from '@angular/platform-browser';
import { CreateCacheDialogComponent } from '@shared/forms';

function routeStub(access: AccessState, query: Record<string, string>): Partial<ActivatedRoute> {
  const route = projectRouteStub('acme', access);
  return { ...route, snapshot: { ...route.snapshot, queryParamMap: convertToParamMap(query) } as never };
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

const subscribeCache = vi.fn((): Observable<unknown> => of(undefined));
const unsubscribeCache = vi.fn((): Observable<unknown> => of(undefined));
const notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function setup(access: AccessState, query: Record<string, string> = {}): ComponentFixture<CacheSubscriptionsComponent> {
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  subscribeCache.mockReset().mockReturnValue(of(undefined));
  unsubscribeCache.mockReset().mockReturnValue(of(undefined));
  notify.success.mockClear();
  notify.failure.mockClear();
  TestBed.configureTestingModule({
    imports: [CacheSubscriptionsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useFactory: () => routeStub(access, query) },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () => of({ id: 'o', display_name: 'Acme' }),
          getSubscribedCaches: () =>
            of([
              { id: 'c', name: 'shared', display_name: 'Shared Cache', mode: 'ReadWrite', status: 'active' },
              { id: 'c2', name: 'staging', display_name: 'Staging Cache', mode: 'ReadWrite', status: 'pending' },
            ]),
          subscribeCache,
          unsubscribeCache,
        },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
      { provide: CachesService, useValue: { getCaches: () => of({ items: [] }), getPublicCaches: () => of([]) } },
      { provide: ConfigService, useValue: { createCache: 'all', canCreate: () => true } },
      { provide: AuthService, useValue: { isAuthenticated: () => true, user: signal({ superuser: false, can_create_cache: true }) } },
    ],
  });
  vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
  return TestBed.createComponent(CacheSubscriptionsComponent);
}

async function settled(fixture: ComponentFixture<CacheSubscriptionsComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('CacheSubscriptionsComponent - access gating', () => {
  it('hides Add Cache and Unsubscribe under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'add cache')).toBeNull();
    expect(findByText(fixture.nativeElement, 'unsubscribe')).toBeNull();
  });

  it('shows but disables Add Cache under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const btn = findByText(fixture.nativeElement, 'add cache') as HTMLButtonElement | null;
    expect(btn).not.toBeNull();
    expect(btn!.getAttribute('aria-disabled')).toBe('true');
  });
});

describe('CacheSubscriptionsComponent - row actions', () => {
  it('opens the cache through its own button rather than the whole row', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.row.is-link')).toBeNull();
    const open = root.querySelector('.row-actions a') as HTMLAnchorElement | null;
    expect(open?.getAttribute('href')).toBe('/caches/shared');
    expect(findByText(root, 'unsubscribe')).not.toBeNull();
  });
});

describe('CacheSubscriptionsComponent - create cache', () => {
  it('offers one Add Cache button whose dialog subscribes or creates', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'create cache')).toBeNull();
    findByText(fixture.nativeElement, 'add cache')!.click();
    await settled(fixture);
    expect(findByText(document.body, 'create new cache')).not.toBeNull();
    expect(findByText(document.body, 'subscribe')).not.toBeNull();
  });

  it('returns to Add Cache with the previous pick when creating is cancelled', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const cmp = fixture.componentInstance;
    cmp.openSubscribeDialog('shared-two');
    cmp.createInsteadOfPick();
    await settled(fixture);
    const dialog = fixture.debugElement.query(By.directive(CreateCacheDialogComponent)).componentInstance as CreateCacheDialogComponent;
    dialog.visible.set(false);
    await settled(fixture);
    expect(cmp.showSubscribeDialog()).toBe(true);
    expect(cmp.newCacheName).toBe('shared-two');
  });

  it('reports a private cache subscription as requested, in the words of the server', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    subscribeCache.mockReturnValue(of('Subscription requested'));
    fixture.componentInstance.newCacheName = 'private';
    fixture.componentInstance.subscribeCache();
    expect(notify.success).toHaveBeenCalledWith('Subscription requested');
  });

  it('suggests caches by display name as well as by name, without the subscribed ones', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    TestBed.inject(CachesService).getCaches = (() =>
      of({ items: [{ name: 'mycache', display_name: 'MyCache' }, { name: 'shared', display_name: 'Shared' }] })) as never;
    await settled(fixture);
    const component = fixture.componentInstance;
    component.openSubscribeDialog();
    component.onCacheSearch({ query: 'My' });
    expect(component.cacheSuggestions()).toEqual([{ label: 'MyCache', value: 'mycache' }]);
    component.onCacheSearch({ query: '' });
    expect(component.cacheSuggestions()).toEqual([{ label: 'MyCache', value: 'mycache' }]);
  });

  it('opens Subscribe with the cache picked on the cache page', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, { subscribe: 'nixpkgs' });
    await settled(fixture);
    expect(fixture.componentInstance.showSubscribeDialog()).toBe(true);
    expect(fixture.componentInstance.newCacheName).toBe('nixpkgs');
  });

  it('drops the picked cache from the address and offers no subscribed cache', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, { subscribe: 'nixpkgs' });
    TestBed.inject(CachesService).getPublicCaches = (() =>
      of([{ name: 'nixpkgs', display_name: 'Nix Packages' }, { name: 'shared', display_name: 'Shared Cache' }])) as never;
    await settled(fixture);
    expect(TestBed.inject(Router).navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { subscribe: null }, queryParamsHandling: 'merge', replaceUrl: true }),
    );
    fixture.componentInstance.onCacheSearch({ query: '' });
    expect(fixture.componentInstance.cacheSuggestions()).toEqual([{ label: 'Nix Packages', value: 'nixpkgs' }]);
  });

  it('opens Subscribe for the picked cache only after the subscriptions load', async () => {
    const subscriptions = new Subject<unknown[]>();
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, { subscribe: 'nixpkgs' });
    TestBed.inject(ProjectsService).getSubscribedCaches = (() => subscriptions) as never;
    await settled(fixture);
    expect(fixture.componentInstance.showSubscribeDialog()).toBe(false);
    subscriptions.next([]);
    subscriptions.complete();
    await settled(fixture);
    expect(fixture.componentInstance.showSubscribeDialog()).toBe(true);
  });

  it('shows the display name of the cache picked on the cache page', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, { subscribe: 'nixpkgs' });
    const publicCaches = new Subject<unknown[]>();
    TestBed.inject(CachesService).getPublicCaches = (() => publicCaches) as never;
    await settled(fixture);
    publicCaches.next([{ name: 'nixpkgs', display_name: 'Nix Packages' }]);
    publicCaches.complete();
    await settled(fixture);
    expect(document.body.querySelector<HTMLInputElement>('#cache-name')?.value).toBe('Nix Packages');
    expect(fixture.componentInstance.newCacheName).toBe('nixpkgs');
  });

  it('reloads the subscriptions on the reload interval', async () => {
    vi.useFakeTimers();
    try {
      const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
      const service = TestBed.inject(ProjectsService) as unknown as { getSubscribedCaches: () => unknown };
      fixture.detectChanges();
      service.getSubscribedCaches = () => of([{ id: 'n', name: 'new', display_name: 'New Cache', mode: 'ReadWrite', status: 'active' }]);
      vi.advanceTimersByTime(15_000);
      fixture.detectChanges();
      expect((fixture.nativeElement as HTMLElement).textContent).toContain('New Cache');
    } finally {
      vi.useRealTimers();
    }
  });

  it('returns to Subscribe with a cache created from it selected', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const cmp = fixture.componentInstance;
    cmp.openSubscribeDialog();
    await settled(fixture);
    findByText(document.body, 'create new cache')!.click();
    await settled(fixture);
    expect(cmp.showSubscribeDialog()).toBe(false);
    const dialog = fixture.debugElement.query(By.directive(CreateCacheDialogComponent)).componentInstance as CreateCacheDialogComponent;
    dialog.created.emit('fresh');
    dialog.visible.set(false);
    await settled(fixture);
    expect(subscribeCache).not.toHaveBeenCalled();
    expect(cmp.showSubscribeDialog()).toBe(true);
    expect(cmp.newCacheName).toBe('fresh');
  });

});

describe('CacheSubscriptionsComponent - pending requests', () => {
  it('marks a pending subscription as awaiting approval', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Staging Cache');
    expect(text).toContain('Pending approval');
  });

  it('offers Withdraw request on a pending row and Unsubscribe on an active one', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'withdraw request')).not.toBeNull();
    expect(findByText(fixture.nativeElement, 'unsubscribe')).not.toBeNull();
  });
});

describe('CacheSubscriptionsComponent - load errors', () => {
  it('shows a failed load as an error, never as the empty state and loads again on Try Again', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const service = TestBed.inject(ProjectsService) as unknown as { getSubscribedCaches: () => unknown };
    service.getSubscribedCaches = () => throwError(() => new Error('database is down'));
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load caches');
    expect(root.textContent).toContain('database is down');
    expect(root.textContent).not.toContain('No cache subscriptions');

    service.getSubscribedCaches = () => of([]);
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    await settled(fixture);
    expect(root.textContent).not.toContain('Failed to load caches');
  });
});

describe('CacheSubscriptionsComponent - unsubscribe', () => {
  it('asks before unsubscribing, naming the cache', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    (findByText(fixture.nativeElement, 'unsubscribe') as HTMLButtonElement).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Shared Cache', confirmLabel: 'Unsubscribe' }));
    expect(unsubscribeCache).toHaveBeenCalledWith('acme', 'shared');
    expect(notify.success).toHaveBeenCalledWith('Removed cache subscription');
  });

  it('asks before withdrawing a pending subscription request', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    (findByText(fixture.nativeElement, 'withdraw request') as HTMLButtonElement).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Staging Cache', confirmLabel: 'Withdraw' }));
    expect(unsubscribeCache).toHaveBeenCalledWith('acme', 'staging');
  });

  it('keeps the subscription when the confirmation is cancelled', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    (findByText(fixture.nativeElement, 'unsubscribe') as HTMLButtonElement).click();
    expect(unsubscribeCache).not.toHaveBeenCalled();
  });
});

describe('CacheSubscriptionsComponent - write failures', () => {
  it('keeps a failed subscribe inside the open dialog', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    subscribeCache.mockReturnValue(throwError(() => new ApiError('Cache nightly is private', 403)));
    await settled(fixture);
    const cmp = fixture.componentInstance;
    cmp.showSubscribeDialog.set(true);
    cmp.newCacheName = 'nightly';
    cmp.subscribeCache();
    fixture.detectChanges();
    expect(document.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Cache nightly is private');
  });

  it('sends one subscribe and stays open while the request runs', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    subscribeCache.mockReturnValue(new Subject<void>());
    await settled(fixture);
    const cmp = fixture.componentInstance;
    cmp.showSubscribeDialog.set(true);
    cmp.newCacheName = 'nightly';
    cmp.subscribeCache();
    fixture.detectChanges();
    cmp.subscribeCache();
    const dialog = document.querySelector('.gr-dialog')!;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(subscribeCache).toHaveBeenCalledTimes(1);
    expect(cmp.showSubscribeDialog()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });
});
