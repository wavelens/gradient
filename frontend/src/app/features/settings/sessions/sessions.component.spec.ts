/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { SessionsComponent } from './sessions.component';
import { UserService } from '@core/services/user.service';
import { AuthService } from '@core/services/auth.service';
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { SessionInfo } from '@core/models';

const CURRENT = { id: 's1', current: true, user_agent: 'Firefox', ip: '10.0.0.1', last_used_at: '2026-10-01T10:00:00', expires_at: '2026-11-01T10:00:00' } as SessionInfo;
const OTHER = { ...CURRENT, id: 's2', current: false };

function createOnePageSessionsResponse(items: SessionInfo[], total = items.length, page = 1) {
  return of({ items, total, page, per_page: 25 });
}

function setup(getSessions: ReturnType<typeof vi.fn>, revokeSession = vi.fn().mockReturnValue(of('ok'))) {
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  const logout = vi.fn().mockReturnValue(of(undefined));
  TestBed.configureTestingModule({
    imports: [SessionsComponent],
    providers: [
      provideRouter([]),
      { provide: UserService, useValue: { getSessions, revokeSession } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
      { provide: AuthService, useValue: { logout, isAuthenticated: () => false } },
    ],
  });
  const fixture = TestBed.createComponent(SessionsComponent);
  fixture.detectChanges();
  return { fixture, notify, confirm, logout, root: fixture.nativeElement as HTMLElement };
}

describe('SessionsComponent', () => {
  it('shows a failed load with Try Again instead of No active sessions', () => {
    const getSessions = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
      .mockReturnValue(createOnePageSessionsResponse([CURRENT]));
    const { fixture, root } = setup(getSessions);
    expect(root.textContent).toContain('Failed to load sessions');
    expect(root.textContent).not.toContain('No active sessions');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getSessions).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('This device');
  });

  it('asks before revoking a session, naming its device and address', () => {
    const revokeSession = vi.fn().mockReturnValue(of('ok'));
    const { notify, root, confirm } = setup(vi.fn().mockReturnValue(createOnePageSessionsResponse([CURRENT, OTHER])), revokeSession);
    revokeButtons(root)[1].click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Revoke session', target: 'Firefox (10.0.0.1)' }));
    expect(revokeSession).toHaveBeenCalledWith('s2');
    expect(notify.success).toHaveBeenCalledWith('Session revoked');
  });

  it('sends no revoke when the confirmation is cancelled', () => {
    const revokeSession = vi.fn().mockReturnValue(of('ok'));
    const { root, confirm } = setup(vi.fn().mockReturnValue(createOnePageSessionsResponse([CURRENT, OTHER])), revokeSession);
    confirm.mockImplementation(() => EMPTY);
    revokeButtons(root)[1].click();
    expect(revokeSession).not.toHaveBeenCalled();
  });

  it('requests the first page and appends the next page’s sessions when Load more is clicked', () => {
    const third = { ...OTHER, id: 's3' };
    const getSessions = vi.fn()
      .mockReturnValueOnce(createOnePageSessionsResponse([CURRENT, OTHER], 3))
      .mockReturnValueOnce(createOnePageSessionsResponse([third], 3, 2));
    const { fixture, root } = setup(getSessions);
    expect(getSessions).toHaveBeenCalledWith(1, 25, undefined);
    expect(root.textContent).toContain('Showing 2 of 3');
    Array.from(root.querySelectorAll<HTMLButtonElement>('gr-load-more button')).find((b) => b.textContent?.includes('Load more'))!.click();
    fixture.detectChanges();
    expect(getSessions).toHaveBeenLastCalledWith(2, 25, undefined);
    expect(revokeButtons(root)).toHaveLength(3);
    expect(root.textContent).not.toContain('Load more');
  });

  it('offers Log out instead of Revoke for the session of this device', () => {
    const { root, confirm, logout } = setup(vi.fn().mockReturnValue(createOnePageSessionsResponse([CURRENT, OTHER])));
    const [current, other] = revokeButtons(root);
    expect(current.textContent).toContain('Log out');
    expect(current.disabled).toBe(false);
    expect(other.textContent).toContain('Revoke');
    current.click();
    expect(logout).toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  });
});

function revokeButtons(root: HTMLElement): HTMLButtonElement[] {
  return Array.from(root.querySelectorAll<HTMLButtonElement>('gr-row button'));
}
