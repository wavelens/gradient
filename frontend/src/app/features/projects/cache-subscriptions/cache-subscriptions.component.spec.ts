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
import { CacheSubscriptionItem, SubscribableCache } from '@core/models';

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
const setCacheSubscriptionSignOutputs = vi.fn((): Observable<unknown> => of(undefined));
const notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function subscription(overrides: Partial<CacheSubscriptionItem>): CacheSubscriptionItem {
  return { id: 'c', name: 'shared', display_name: 'Shared Cache', mode: 'ReadWrite', status: 'active', managed: false, sign_outputs: true, approvers: [], ...overrides };
}

function subscribable(overrides: Partial<SubscribableCache>): SubscribableCache {
  return { name: 'mycache', display_name: 'MyCache', public: false, needs_approval: false, approvers: [], ...overrides };
}

function setup(access: AccessState, query: Record<string, string> = {}): ComponentFixture<CacheSubscriptionsComponent> {
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  subscribeCache.mockReset().mockReturnValue(of(undefined));
  unsubscribeCache.mockReset().mockReturnValue(of(undefined));
  setCacheSubscriptionSignOutputs.mockReset().mockReturnValue(of(undefined));
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
              subscription({}),
              subscription({ id: 'c2', name: 'staging', display_name: 'Staging Cache', status: 'pending' }),
            ]),
          getSubscribableCaches: () => of([]),
          subscribeCache,
          unsubscribeCache,
          setCacheSubscriptionSignOutputs,
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

  it('hides Add Cache and Unsubscribe under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'add cache')).toBeNull();
    expect(findByText(fixture.nativeElement, 'unsubscribe')).toBeNull();
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

  it('names the cache admins when the server records a request instead of a subscription', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    TestBed.inject(ProjectsService).getSubscribableCaches = (() =>
      of([subscribable({ name: 'private', needs_approval: true, approvers: ['admin', 'dave'] })])) as never;
    await settled(fixture);
    subscribeCache.mockReturnValue(of('Subscription requested'));
    fixture.componentInstance.openSubscribeDialog();
    fixture.componentInstance.newCacheName = 'private';
    fixture.componentInstance.subscribeCache();
    expect(notify.success).toHaveBeenCalledWith('Subscription requested. Approval from a cache admin is pending: admin, dave');
    expect(fixture.componentInstance.showSubscribeDialog()).toBe(false);
  });

  it('passes on the words of the server for a direct subscription', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    subscribeCache.mockReturnValue(of('Subscribed to cache'));
    fixture.componentInstance.newCacheName = 'mycache';
    fixture.componentInstance.subscribeCache();
    expect(notify.success).toHaveBeenCalledWith('Subscribed to cache');
  });

  it('sends Read & Write by default and the mode picked in the dialog', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const cmp = fixture.componentInstance;
    cmp.openSubscribeDialog();
    cmp.newCacheName = 'mycache';
    cmp.subscribeCache();
    expect(subscribeCache).toHaveBeenLastCalledWith('acme', 'mycache', 'ReadWrite');
    cmp.openSubscribeDialog();
    cmp.newCacheName = 'mycache';
    cmp.selectedSubscriptionMode.set('ReadOnly');
    cmp.subscribeCache();
    expect(subscribeCache).toHaveBeenLastCalledWith('acme', 'mycache', 'ReadOnly');
  });

  it('suggests subscribable caches by display name or name, with owner team, visibility and who approves', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    TestBed.inject(ProjectsService).getSubscribableCaches = (() =>
      of([
        subscribable({ owner_team: 'myteam', owner_team_display_name: 'My Team' }),
        subscribable({ name: 'nixpkgs', display_name: 'Nix Packages', public: true, needs_approval: true, approvers: ['admin', 'dave'] }),
        subscribable({ name: 'hidden', display_name: 'Hidden', needs_approval: true }),
      ])) as never;
    await settled(fixture);
    const component = fixture.componentInstance;
    component.openSubscribeDialog();
    component.onCacheSearch({ query: 'My' });
    expect(component.cacheSuggestions()).toEqual([{ label: 'MyCache (mycache)', value: 'mycache', textBelowLabel: 'My Team · Private · Write access' }]);
    component.onCacheSearch({ query: 'nixpkgs' });
    expect(component.cacheSuggestions()).toEqual([
      { label: 'Nix Packages (nixpkgs)', value: 'nixpkgs', textBelowLabel: 'Public · Pending approval from admin, dave' },
    ]);
    component.onCacheSearch({ query: 'hidden' });
    expect(component.cacheSuggestions()[0].textBelowLabel).toBe('Private · Ask a cache administrator for approval.');
  });

  it('opens Subscribe with the cache picked on the cache page', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, { subscribe: 'nixpkgs' });
    await settled(fixture);
    expect(fixture.componentInstance.showSubscribeDialog()).toBe(true);
    expect(fixture.componentInstance.newCacheName).toBe('nixpkgs');
  });

  it('drops the picked cache from the address and lists the caches this project can subscribe to', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true }, { subscribe: 'nixpkgs' });
    const getSubscribableCaches = vi.fn(() => of([subscribable({ name: 'nixpkgs', display_name: 'Nix Packages', public: true })]));
    TestBed.inject(ProjectsService).getSubscribableCaches = getSubscribableCaches as never;
    await settled(fixture);
    expect(TestBed.inject(Router).navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { subscribe: null }, queryParamsHandling: 'merge', replaceUrl: true }),
    );
    expect(getSubscribableCaches).toHaveBeenCalledWith('acme');
    fixture.componentInstance.onCacheSearch({ query: '' });
    expect(fixture.componentInstance.cacheSuggestions().map((s) => s.value)).toEqual(['nixpkgs']);
  });

  it('shows a failed load of the subscribable caches inside the dialog', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    TestBed.inject(ProjectsService).getSubscribableCaches = (() => throwError(() => new ApiError('Project acme is gone', 404))) as never;
    await settled(fixture);
    fixture.componentInstance.openSubscribeDialog();
    expect(fixture.componentInstance.errorMessage()).toBe('Project acme is gone');
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
    TestBed.inject(ProjectsService).getSubscribableCaches = (() => publicCaches) as never;
    await settled(fixture);
    publicCaches.next([subscribable({ name: 'nixpkgs', display_name: 'Nix Packages' })]);
    publicCaches.complete();
    await settled(fixture);
    expect(document.body.querySelector<HTMLInputElement>('#cache-name')?.value).toBe('Nix Packages (nixpkgs)');
    expect(fixture.componentInstance.newCacheName).toBe('nixpkgs');
  });

  it('reloads the subscriptions on the reload interval', async () => {
    vi.useFakeTimers();
    try {
      const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
      const service = TestBed.inject(ProjectsService) as unknown as { getSubscribedCaches: () => unknown };
      fixture.detectChanges();
      service.getSubscribedCaches = () => of([subscription({ id: 'n', name: 'new', display_name: 'New Cache' })]);
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
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Shared Cache (shared)', confirmLabel: 'Unsubscribe' }));
    expect(unsubscribeCache).toHaveBeenCalledWith('acme', 'shared');
    expect(notify.success).toHaveBeenCalledWith('Removed cache subscription');
  });

  it('asks before withdrawing a pending subscription request', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    (findByText(fixture.nativeElement, 'withdraw request') as HTMLButtonElement).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Staging Cache (staging)', confirmLabel: 'Withdraw' }));
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

describe('CacheSubscriptionsComponent - subscriptions from the server configuration', () => {
  it('marks a managed row of an unmanaged project with a lock, hides Unsubscribe and disables the sign setting', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    TestBed.inject(ProjectsService).getSubscribedCaches = (() => of([subscription({ managed: true })])) as never;
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-row app-server-configuration-managed-lock-button')).not.toBeNull();
    expect(findByText(root, 'unsubscribe')).toBeNull();
    expect(root.querySelector<HTMLInputElement>('#sign-pushed-paths-shared')!.disabled).toBe(true);
  });

  it('leaves the lock to the title chip when the whole project is managed', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    TestBed.inject(ProjectsService).getSubscribedCaches = (() => of([subscription({ managed: true })])) as never;
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-row app-server-configuration-managed-lock-button')).toBeNull();
    expect(root.querySelector('app-settings-access-state-badge')?.textContent).toContain('Managed');
  });

  it('leaves an unmanaged row editable', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(findByText(root, 'unsubscribe')!.getAttribute('aria-disabled')).toBeNull();
    expect(root.querySelector<HTMLInputElement>('#sign-pushed-paths-shared')!.disabled).toBe(false);
  });
});

describe('CacheSubscriptionsComponent - sign pushed paths', () => {
  it('saves the sign setting on change and shows the saved value', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const checkbox = (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>('#sign-pushed-paths-shared')!;
    expect(checkbox.checked).toBe(true);
    expect(checkbox.labels?.[0]?.textContent).toBe('Sign pushed paths with the cache key');
    checkbox.click();
    await settled(fixture);
    expect(setCacheSubscriptionSignOutputs).toHaveBeenCalledWith('acme', 'shared', false);
    expect(notify.success).toHaveBeenCalledWith('Cache subscription saved');
    expect(fixture.componentInstance.caches()[0].sign_outputs).toBe(false);
    expect(checkbox.checked).toBe(false);
  });

  it('locks the setting during the request and shows the saved value again after a failure', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const response = new Subject<unknown>();
    setCacheSubscriptionSignOutputs.mockReturnValue(response);
    await settled(fixture);
    const checkbox = (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>('#sign-pushed-paths-shared')!;
    checkbox.click();
    await settled(fixture);
    expect(checkbox.checked).toBe(false);
    expect(checkbox.disabled).toBe(true);
    const error = new ApiError('Subscription is managed', 409);
    response.error(error);
    await settled(fixture);
    expect(notify.failure).toHaveBeenCalledWith("Couldn't save cache subscription", error);
    expect(checkbox.checked).toBe(true);
    expect(checkbox.disabled).toBe(false);
  });

  it('shows the sign setting to read-only members without letting them change it', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    const checkbox = (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>('#sign-pushed-paths-shared')!;
    expect(checkbox.checked).toBe(true);
    expect(checkbox.disabled).toBe(true);
  });
});

describe('CacheSubscriptionsComponent - pending request approvers', () => {
  it('names the cache admins who can approve a pending request', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    TestBed.inject(ProjectsService).getSubscribedCaches = (() =>
      of([subscription({ name: 'staging', display_name: 'Staging Cache', status: 'pending', approvers: ['admin', 'dave'] })])) as never;
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.row-meta')?.textContent).toContain('Cache admins: admin, dave');
    expect(root.querySelector('#sign-pushed-paths-staging')).toBeNull();
  });
});
