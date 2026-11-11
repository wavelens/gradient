/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { CachesService } from '@core/services/caches.service';
import { NotifyService } from '@core/services/notify.service';
import { NarSummary } from '@core/models';
import { CacheNarsPinScopeDialogComponent } from './cache-nars-pin-scope-dialog.component';

const unpinnedCacheNar: NarSummary = {
  hash: 'abc123',
  store_path: '/nix/store/abc123-hello',
  package: 'hello',
  nar_size: 1024,
  file_size: 512,
  created_at: '2026-10-05T10:00:00',
  last_fetched_at: null,
};
const pinnedNarSummary: NarSummary = { ...unpinnedCacheNar, pinned_at: '2026-10-06T10:00:00', pinned_by_username: 'alice' };

function setupPinScopeDialogWithMockedServices(row: NarSummary) {
  const mockedCachesService = {
    pinCacheNar: vi.fn(() => of({ pin_state_changed_path_count: 3 })),
    unpinCacheNar: vi.fn(() => of({ pin_state_changed_path_count: 1 })),
  };
  TestBed.configureTestingModule({
    imports: [CacheNarsPinScopeDialogComponent],
    providers: [
      { provide: CachesService, useValue: mockedCachesService },
      { provide: NotifyService, useValue: { success: vi.fn(), error: vi.fn() } },
    ],
  });
  const fixture = TestBed.createComponent(CacheNarsPinScopeDialogComponent);
  fixture.componentRef.setInput('cacheName', 'main');
  fixture.componentRef.setInput('row', row);
  const dialogClosedSpy = vi.fn();
  fixture.componentInstance.closed.subscribe(dialogClosedSpy);
  fixture.detectChanges();
  const openDialogPanel = () => document.querySelector<HTMLElement>('.gr-dialog');
  const findButtonByVisibleLabel = (label: string) =>
    Array.from(openDialogPanel()?.querySelectorAll<HTMLButtonElement>('button') ?? []).find((b) => b.textContent?.trim() === label);
  return { fixture, mockedCachesService, dialogClosedSpy, findButtonByVisibleLabel };
}

describe('CacheNarsPinScopeDialogComponent', () => {
  it('pin package plus all referenced paths for selected scope', () => {
    const { fixture, mockedCachesService, dialogClosedSpy, findButtonByVisibleLabel } = setupPinScopeDialogWithMockedServices(unpinnedCacheNar);
    findButtonByVisibleLabel('With all referenced paths')!.click();
    fixture.detectChanges();
    findButtonByVisibleLabel('Pin')!.click();
    fixture.detectChanges();
    expect(mockedCachesService.pinCacheNar).toHaveBeenCalledWith('main', 'abc123', true);
    expect(mockedCachesService.unpinCacheNar).not.toHaveBeenCalled();
    expect(dialogClosedSpy).toHaveBeenCalled();
  });

  it('unpin only this path of a pinned package by default', () => {
    const { fixture, mockedCachesService, findButtonByVisibleLabel } = setupPinScopeDialogWithMockedServices(pinnedNarSummary);
    expect(findButtonByVisibleLabel('Pin')).toBeUndefined();
    findButtonByVisibleLabel('Unpin')!.click();
    fixture.detectChanges();
    expect(mockedCachesService.unpinCacheNar).toHaveBeenCalledWith('main', 'abc123', false);
    expect(mockedCachesService.pinCacheNar).not.toHaveBeenCalled();
  });

  it('cancel sent nothing and closed the dialog', () => {
    const { mockedCachesService, dialogClosedSpy, findButtonByVisibleLabel } = setupPinScopeDialogWithMockedServices(unpinnedCacheNar);
    findButtonByVisibleLabel('Cancel')!.click();
    expect(mockedCachesService.pinCacheNar).not.toHaveBeenCalled();
    expect(dialogClosedSpy).toHaveBeenCalled();
  });
});
