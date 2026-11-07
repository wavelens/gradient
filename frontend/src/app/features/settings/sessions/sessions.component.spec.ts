/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { SessionsComponent } from './sessions.component';
import { UserService } from '@core/services/user.service';
import { ApiError } from '@core/services/api.service';
import { SessionInfo } from '@core/models';

const CURRENT = { id: 's1', current: true, user_agent: 'Firefox', ip: '10.0.0.1', last_used_at: '2026-10-01T10:00:00', expires_at: '2026-11-01T10:00:00' } as SessionInfo;
const OTHER = { ...CURRENT, id: 's2', current: false };

function setup(getSessions: ReturnType<typeof vi.fn>, revokeSession = vi.fn().mockReturnValue(of('ok'))) {
  TestBed.configureTestingModule({
    imports: [SessionsComponent],
    providers: [provideRouter([]), { provide: UserService, useValue: { getSessions, revokeSession } }],
  });
  const fixture = TestBed.createComponent(SessionsComponent);
  fixture.detectChanges();
  return { fixture, root: fixture.nativeElement as HTMLElement };
}

describe('SessionsComponent', () => {
  it('shows a failed load with Try Again instead of No active sessions', () => {
    const getSessions = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)))
      .mockReturnValue(of([CURRENT]));
    const { fixture, root } = setup(getSessions);
    expect(root.textContent).toContain('Failed to load sessions');
    expect(root.textContent).not.toContain('No active sessions');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getSessions).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('This device');
  });

  it('clears an old revoke error once the next revoke succeeds', () => {
    const revokeSession = vi.fn()
      .mockReturnValueOnce(throwError(() => new ApiError('boom', 500)))
      .mockReturnValue(of('ok'));
    const { fixture } = setup(vi.fn().mockReturnValue(of([CURRENT, OTHER])), revokeSession);
    const component = fixture.componentInstance;
    component.revoke(OTHER);
    expect(component.errorMessage()).not.toBeNull();
    component.revoke(OTHER);
    expect(component.errorMessage()).toBeNull();
  });
});
