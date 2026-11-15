/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Observable, of, throwError } from 'rxjs';
import { type Mock, vi } from 'vitest';
import type { UserInfoResponse, YearlyRecapResponse } from '@core/models';
import { ApiError, ApiService } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { WhatsNewService } from '@core/whats-new/whats-new.service';
import { YearlyRecapAutoOpenService } from './yearly-recap-auto-open.service';

const RECAP: YearlyRecapResponse = {
  year: 2026,
  created_at: '2026-12-01T00:00:00',
  data_since: '2026-01-01',
  opened: false,
  statistics: {},
};

const USER: UserInfoResponse = { id: 'u1', username: 'alice', name: 'Alice', email: 'a@x', superuser: false, last_seen_version: '1.0.0', can_create_cache: true };

function createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(
  recap: YearlyRecapResponse | null,
  { whatsNewOpen = false, put = vi.fn(() => of('Marked')) }: { whatsNewOpen?: boolean; put?: Mock<() => Observable<string>> } = {},
) {
  const user = signal<UserInfoResponse | null>(null);
  const whatsNew = { open: signal(whatsNewOpen) };
  const get = vi.fn(() => of(recap));
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    providers: [
      { provide: ApiService, useValue: { get, put } },
      { provide: AuthService, useValue: { user } },
      { provide: NotifyService, useValue: notify },
      { provide: WhatsNewService, useValue: whatsNew },
    ],
  });
  const service = TestBed.inject(YearlyRecapAutoOpenService);
  user.set(USER);
  TestBed.tick();
  return { service, user, whatsNew, get, put, notify };
}

describe('YearlyRecapAutoOpenService', () => {
  it("open the recap once when the user hasn't opened the recap yet", () => {
    const { service, get } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(RECAP);
    expect(get).toHaveBeenCalledWith('user/yearly-recap');
    expect(service.open()).toBe(true);
    service.closeRecapSlideShowAndMarkOpenedOnServer();
    TestBed.tick();
    expect(service.open()).toBe(false);
  });

  it('keep the recap closed when the user already opened the recap', () => {
    const { service } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices({ ...RECAP, opened: true });
    expect(service.recap()?.year).toBe(2026);
    expect(service.open()).toBe(false);
  });

  it('wait for the release notes to close before opening the recap', () => {
    const { service, whatsNew } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(RECAP, { whatsNewOpen: true });
    expect(service.open()).toBe(false);
    whatsNew.open.set(false);
    TestBed.tick();
    expect(service.open()).toBe(true);
  });

  it('mark the recap as opened once after two closes', () => {
    const { service, put } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(RECAP);
    service.closeRecapSlideShowAndMarkOpenedOnServer();
    service.reopenRecapSlideShowFromDashboardCard();
    service.closeRecapSlideShowAndMarkOpenedOnServer();
    expect(put).toHaveBeenCalledOnce();
    expect(put).toHaveBeenCalledWith('user/yearly-recap');
    expect(service.recap()?.opened).toBe(true);
  });

  it('report a failed attempt to mark the recap as opened', () => {
    const failure = new ApiError('database is down', 500);
    const { service, notify } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(RECAP, { put: vi.fn(() => throwError(() => failure)) });
    service.closeRecapSlideShowAndMarkOpenedOnServer();
    expect(notify.failure).toHaveBeenCalledWith("Couldn't mark Gradient Wrapped as opened", failure);
  });

  it('show nothing outside the display period', () => {
    const { service } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(null);
    expect(service.recap()).toBeNull();
    service.reopenRecapSlideShowFromDashboardCard();
    expect(service.open()).toBe(false);
  });

  it('reopen an opened recap from the dashboard card without marking the recap again', () => {
    const { service, put } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices({ ...RECAP, opened: true });
    service.reopenRecapSlideShowFromDashboardCard();
    expect(service.open()).toBe(true);
    service.closeRecapSlideShowAndMarkOpenedOnServer();
    expect(service.open()).toBe(false);
    expect(put).not.toHaveBeenCalled();
  });

  it('open a recap revealed while the tab stays open and close it when the display period ends', () => {
    const { service, get } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(null);
    get.mockReturnValue(of(RECAP));
    service.refreshCurrentYearlyRecapAndResetAutoOpenStateOnYearChange().subscribe();
    TestBed.tick();
    expect(service.recap()?.year).toBe(2026);
    expect(service.open()).toBe(true);
    get.mockReturnValue(of(null));
    service.refreshCurrentYearlyRecapAndResetAutoOpenStateOnYearChange().subscribe();
    TestBed.tick();
    expect(service.recap()).toBeNull();
    expect(service.open()).toBe(false);
  });

  it('keep the open state when a refresh returns the same recap year', () => {
    const { service } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(RECAP);
    service.closeRecapSlideShowAndMarkOpenedOnServer();
    service.refreshCurrentYearlyRecapAndResetAutoOpenStateOnYearChange().subscribe();
    TestBed.tick();
    expect(service.open()).toBe(false);
    expect(service.recap()?.opened).toBe(true);
  });

  it('reload the recap for the next user to sign in', () => {
    const { service, user, get } = createYearlyRecapAutoOpenServiceWithSignedInUserAndStubbedServices(RECAP);
    user.set(null);
    TestBed.tick();
    expect(service.recap()).toBeNull();
    expect(service.open()).toBe(false);
    user.set({ ...USER, id: 'u2' });
    TestBed.tick();
    expect(get).toHaveBeenCalledTimes(2);
    expect(service.open()).toBe(true);
  });
});
