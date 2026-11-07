/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { cacheAccessData } from '@core/resolvers/access-route.testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { CacheSettingsComponent } from './cache-settings.component';
import { CachesService } from '@core/services/caches.service';
import { AccessState } from '@core/models/access.model';
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';

const notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function cacheFor(access: AccessState) {
  return {
    id: 'c',
    name: 'demo',
    display_name: 'Demo',
    description: '',
    active: true,
    priority: 50,
    max_storage_gb: 0,
    pull_through: true,
    public: false,
    managed: access.managed,
    can_edit: access.canEdit,
  };
}

function activatedRouteStub(access: AccessState): ActivatedRoute {
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'demo' }) },
    data: of({}),
    parent: { data: of(cacheAccessData(access, cacheFor(access))) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(access: AccessState): ComponentFixture<CacheSettingsComponent> {
  notify.success.mockClear();
  notify.failure.mockClear();
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  TestBed.configureTestingModule({
    imports: [CacheSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(access) },
      { provide: CachesService, useValue: { getCache: () => of(cacheFor(access)) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
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

  it('keeps Deactivate usable under state-managed access, since state restores it on restart', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const toggle = findByText(fixture.nativeElement, 'deactivate') as HTMLButtonElement | null;
    expect(toggle).not.toBeNull();
    expect(toggle!.disabled).toBe(false);
  });

  it('always reaches upstream caches and members, even when state-managed', () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    const hrefs = (Array.from(fixture.nativeElement.querySelectorAll('a')) as HTMLAnchorElement[]).map(
      (el) => el.getAttribute('href'),
    );
    expect(hrefs).toContain('/caches/demo/upstream-caches');
    expect(hrefs).toContain('/caches/demo/members-roles');
  });
});

describe('CacheSettingsComponent - write failures', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('reports a failed deactivate instead of logging it', () => {
    const fixture = setup(admin);
    const failure = new ApiError('The server failed to process the request. Try again later.', 500);
    const service = TestBed.inject(CachesService) as unknown as { deactivateCache: () => unknown };
    service.deactivateCache = () => throwError(() => failure);
    findByText(fixture.nativeElement, 'deactivate')!.click();
    expect(notify.failure).toHaveBeenCalledWith('Cache deactivation failed', failure);
    expect(fixture.componentInstance.toggling()).toBe(false);
  });

  it('asks before deleting, naming the cache, then leaves for the cache list', () => {
    const fixture = setup(admin);
    const service = TestBed.inject(CachesService) as unknown as { deleteCache: () => unknown };
    service.deleteCache = vi.fn(() => of(undefined));
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    findByText(fixture.nativeElement, 'delete cache')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete cache', target: 'Demo' }));
    expect(service.deleteCache).toHaveBeenCalledWith('demo');
    expect(notify.success).toHaveBeenCalledWith('Cache deleted');
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/caches');
  });

  it('sends no delete when the confirmation is cancelled', () => {
    const fixture = setup(admin);
    confirm.mockImplementation(() => EMPTY);
    const service = TestBed.inject(CachesService) as unknown as { deleteCache: () => unknown };
    service.deleteCache = vi.fn(() => of(undefined));
    findByText(fixture.nativeElement, 'delete cache')!.click();
    expect(service.deleteCache).not.toHaveBeenCalled();
  });

  it('sends one save when Save Changes is clicked twice', () => {
    const fixture = setup(admin);
    const service = TestBed.inject(CachesService) as unknown as { updateCache: () => unknown; setCachePrivate: () => unknown };
    service.updateCache = vi.fn(() => new Subject<void>());
    service.setCachePrivate = () => of(undefined);
    fixture.componentInstance.formData.priority = 40;
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
    const save = findByText(fixture.nativeElement, 'save changes')!;
    save.click();
    fixture.detectChanges();
    save.click();
    expect(service.updateCache).toHaveBeenCalledTimes(1);
  });
});

describe('CacheSettingsComponent - edits', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

  function stubWrites(fixture: ComponentFixture<CacheSettingsComponent>) {
    const service = TestBed.inject(CachesService) as unknown as Record<string, unknown>;
    const updateCache = vi.fn(() => of('ok'));
    const setCachePublic = vi.fn(() => of('ok'));
    Object.assign(service, { updateCache, setCachePublic, setCachePrivate: vi.fn(() => of('ok')) });
    fixture.detectChanges();
    return { updateCache, setCachePublic };
  }

  function edit(fixture: ComponentFixture<CacheSettingsComponent>, change: Record<string, unknown>) {
    Object.assign(fixture.componentInstance.formData, change);
    fixture.componentRef.changeDetectorRef.markForCheck();
    fixture.detectChanges();
  }

  const saveButton = (fixture: ComponentFixture<CacheSettingsComponent>) =>
    findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement;

  it('keeps Save Changes disabled and says why until a field changes', () => {
    const fixture = setup(admin);
    expect(saveButton(fixture).disabled).toBe(true);
    expect(fixture.nativeElement.textContent).toContain('No changes to save.');
    edit(fixture, { description: 'Shared Build' });
    expect(saveButton(fixture).disabled).toBe(false);
    expect(fixture.nativeElement.textContent).not.toContain('No changes to save.');
  });

  it('sends only the changed fields and skips an unchanged visibility', () => {
    const fixture = setup(admin);
    const { updateCache, setCachePublic } = stubWrites(fixture);
    edit(fixture, { priority: 40 });
    saveButton(fixture).click();
    expect(updateCache).toHaveBeenCalledWith('demo', { priority: 40 });
    expect(setCachePublic).not.toHaveBeenCalled();
  });

  it('changes only the visibility when nothing else changed', () => {
    const fixture = setup(admin);
    const { updateCache, setCachePublic } = stubWrites(fixture);
    edit(fixture, { public: true });
    saveButton(fixture).click();
    expect(updateCache).not.toHaveBeenCalled();
    expect(setCachePublic).toHaveBeenCalledWith('demo');
  });

  it.each([
    { field: 'max_storage_gb', value: 0.5, message: 'Enter 0 for no limit' },
    { field: 'max_storage_gb', value: null, message: 'Enter 0 for no limit' },
    { field: 'priority', value: 300, message: 'Enter an integer from 0 to 255.' },
    { field: 'local_priority', value: -5, message: 'Enter an integer from 0 to 255.' },
    { field: 'display_name', value: '  ', message: 'Enter a display name.' },
  ])('shows why $field = $value is refused and sends nothing', ({ field, value, message }) => {
    const fixture = setup(admin);
    const { updateCache } = stubWrites(fixture);
    edit(fixture, { [field]: value });
    expect(fixture.nativeElement.textContent).toContain(message);
    saveButton(fixture).click();
    fixture.componentInstance.saveSettings();
    expect(updateCache).not.toHaveBeenCalled();
  });

  it('drops the saved banner as soon as the form changes again', () => {
    const fixture = setup(admin);
    stubWrites(fixture);
    edit(fixture, { priority: 40 });
    saveButton(fixture).click();
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('Changes saved.');
    edit(fixture, { priority: 30 });
    expect(fixture.nativeElement.textContent).not.toContain('Changes saved.');
  });

  it('asks before leaving only with unsaved edits', () => {
    const fixture = setup(admin);
    confirm.mockImplementation(() => EMPTY);
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();
    edit(fixture, { display_name: 'Renamed' });
    expect(fixture.componentInstance.canDeactivate()).not.toBe(true);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Cache Settings' }));
  });
});
