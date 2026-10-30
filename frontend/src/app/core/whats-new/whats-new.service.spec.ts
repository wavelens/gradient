/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { vi } from 'vitest';
import type { UserInfoResponse } from '@core/models';
import { ApiService } from '@core/services/api.service';
import { AuthService } from '@core/services/auth.service';
import { environment } from '@environments/environment';
import { releaseLine } from './version';
import { RELEASE_NOTES_SOURCE, WhatsNewService } from './whats-new.service';

const CURRENT = environment.version;
const LINE = releaseLine(CURRENT);
const NOTES = [
  { version: '1.9', pages: [{ title: 'Old', html: '<p>old</p>' }] },
  { version: '99.0', pages: [{ title: 'Future', html: '<p>future</p>' }] },
  { version: LINE, pages: [{ title: 'Current', html: '<p>current</p>' }] },
];

function setup(lastSeen: string | null) {
  const put = vi.fn(() => of('Version stored'));
  const user = signal<UserInfoResponse | null>(null);
  TestBed.configureTestingModule({
    providers: [
      { provide: ApiService, useValue: { put } },
      { provide: AuthService, useValue: { user } },
      { provide: RELEASE_NOTES_SOURCE, useValue: NOTES },
    ],
  });
  const service = TestBed.inject(WhatsNewService);
  user.set({ id: 'u1', username: 'alice', name: 'Alice', email: 'a@x', superuser: false, last_seen_version: lastSeen });
  TestBed.tick();
  return { service, put };
}

describe('WhatsNewService', () => {
  it('skips the notes once for a new account and remembers the running version', () => {
    const { service, put } = setup(null);
    expect(service.open()).toBe(false);
    expect(put).toHaveBeenCalledWith('user/whats-new', { version: CURRENT });
  });

  it('shows every released note newer than the last seen version, newest first', () => {
    const { service, put } = setup('1.0.0');
    expect(service.open()).toBe(true);
    expect(service.notes().map((n) => n.version)).toEqual([LINE, '1.9']);
    expect(put).not.toHaveBeenCalled();
  });

  it('ignores a patch upgrade within the release line the user already saw', () => {
    const { service } = setup(`${LINE}.0-rc1`);
    expect(service.open()).toBe(false);
  });

  it('remembers the running version once the notes are closed', () => {
    const { service, put } = setup('1.9.4');
    expect(service.notes().map((n) => n.version)).toEqual([LINE]);
    service.close();
    expect(service.open()).toBe(false);
    expect(put).toHaveBeenCalledWith('user/whats-new', { version: CURRENT });
  });

  it('stays closed when the user saw the running version and reopens on demand', () => {
    const { service, put } = setup(CURRENT);
    expect(service.open()).toBe(false);
    service.showAll();
    expect(service.notes().map((n) => n.version)).toEqual([LINE, '1.9']);
    service.close();
    expect(put).not.toHaveBeenCalled();
  });
});
