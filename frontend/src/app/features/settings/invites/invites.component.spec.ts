/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { InvitesComponent } from './invites.component';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { InviteItem } from '@core/models';

const projectInvite: InviteItem = {
  kind: 'project',
  token: 'tok-project',
  scope: 'wavelens',
  scope_display_name: 'Wavelens',
  role: 'Admin',
  invited_by: 'alice',
  created_at: '2026-09-05T12:00:00',
  expires_at: '2026-09-12T12:00:00',
};

const teamInvite: InviteItem = {
  ...projectInvite,
  kind: 'team',
  token: 'tok-team',
  scope: 'platform',
  scope_display_name: 'Platform',
  role: 'Member',
};

function setup(invites: InviteItem[], queryToken: string | null = null, getInvites: () => Observable<InviteItem[]> = () => of(invites)) {
  const accept = vi.fn().mockReturnValue(of('Invitation accepted'));
  const decline = vi.fn().mockReturnValue(of('Invitation declined'));
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());

  TestBed.configureTestingModule({
    imports: [InvitesComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: {
          snapshot: {
            queryParamMap: convertToParamMap(queryToken ? { token: queryToken } : {}),
          },
        },
      },
      {
        provide: UserService,
        useValue: {
          getInvites,
          acceptInvite: accept,
          declineInvite: decline,
        },
      },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });

  const fixture: ComponentFixture<InvitesComponent> = TestBed.createComponent(InvitesComponent);
  fixture.detectChanges();
  return { fixture, accept, decline, notify, confirm };
}

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent || '').trim().includes(text),
  ) as HTMLButtonElement | undefined;
}

describe('InvitesComponent - loading', () => {
  it('shows a failed load with Try Again instead of No pending invitations', () => {
    const getInvites = vi.fn().mockReturnValueOnce(throwError(() => new Error('database is down'))).mockReturnValue(of([projectInvite]));
    const { fixture } = setup([], null, getInvites);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load invitations');
    expect(root.textContent).not.toContain('No pending invitations');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getInvites).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('Wavelens');
  });
});

describe('InvitesComponent', () => {
  it('lists a team invite as a team', () => {
    const { fixture } = setup([teamInvite]);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Platform');
    expect(text).toContain('team');
  });

  it('lists a pending invite with its scope and role', () => {
    const { fixture } = setup([projectInvite]);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Wavelens');
    expect(text).toContain('Admin');
  });

  it('shows an empty state when there is nothing to accept', () => {
    const { fixture } = setup([]);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('No pending invitations');
  });

  it('accepts an invite by token when Accept is clicked', () => {
    const { fixture, accept } = setup([projectInvite]);
    buttonByText(fixture.nativeElement, 'Accept')?.click();
    expect(accept).toHaveBeenCalledWith('tok-project');
  });

  it('asks before declining, naming what the invite is for, then declines by token', () => {
    const { fixture, decline, notify, confirm } = setup([projectInvite]);
    buttonByText(fixture.nativeElement, 'Decline')?.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Wavelens', confirmLabel: 'Decline' }));
    expect(decline).toHaveBeenCalledWith('tok-project');
    expect(notify.success).toHaveBeenCalledWith('Invitation declined');
  });

  it('keeps the invite when declining is cancelled', () => {
    const { fixture, decline, confirm } = setup([projectInvite]);
    confirm.mockImplementation(() => EMPTY);
    buttonByText(fixture.nativeElement, 'Decline')?.click();
    expect(decline).not.toHaveBeenCalled();
  });

  it('reports an accepted invitation', () => {
    const { fixture, notify } = setup([projectInvite]);
    buttonByText(fixture.nativeElement, 'Accept')?.click();
    expect(notify.success).toHaveBeenCalledWith('Invitation accepted');
  });

  it('accepts the token carried in the query string on load', () => {
    const { accept } = setup([projectInvite], 'tok-project');
    expect(accept).toHaveBeenCalledWith('tok-project');
  });
});
