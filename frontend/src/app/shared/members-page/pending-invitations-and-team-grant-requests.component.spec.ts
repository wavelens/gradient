/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { EMPTY, Observable, of } from 'rxjs';
import type { MemberPlace } from '@shared/access';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { TeamsService } from '@core/services/teams.service';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { PendingInvitationsAndTeamGrantRequestsComponent } from './pending-invitations-and-team-grant-requests.component';

const invitation = (user: string) => ({ user, name: user, role: 'View', created_at: '2026-11-01T10:00:00', expires_at: '2026-11-08T10:00:00' });

function setup(kind: MemberPlace, confirmAnswer: 'confirm' | 'cancel' = 'confirm') {
  const requests = {
    invitations: vi.fn(() => of([invitation('bob'), invitation('erin')])),
    revokeInvitation: vi.fn(() => of('ok')),
  };
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => (confirmAnswer === 'confirm' ? request.action() : EMPTY));
  TestBed.configureTestingModule({
    imports: [PendingInvitationsAndTeamGrantRequestsComponent],
    providers: [
      provideRouter([]),
      { provide: TeamsService, useValue: { list: () => of([]), projectGrants: () => of([]), cacheGrants: () => of([]) } },
      { provide: AuthService, useValue: { isAuthenticated: () => true, user: signal({ superuser: false }) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(PendingInvitationsAndTeamGrantRequestsComponent);
  fixture.componentRef.setInput('kind', kind);
  fixture.componentRef.setInput('name', 'acme');
  fixture.componentRef.setInput('requests', requests);
  fixture.componentRef.setInput('access', { managed: false, canEdit: true, canTrigger: true });
  fixture.detectChanges();
  const root = fixture.nativeElement as HTMLElement;
  const revokeButton = (user: string) => root.querySelector<HTMLButtonElement>(`button[aria-label="Revoke invitation for ${user}"]`)!;
  return { fixture, root, requests, notify, revokeButton };
}

describe('PendingInvitationsAndTeamGrantRequestsComponent', () => {
  it('drops a revoked invitation from the list', () => {
    const { fixture, root, requests, notify, revokeButton } = setup('project');
    revokeButton('bob').click();
    fixture.detectChanges();
    expect(requests.revokeInvitation).toHaveBeenCalledWith('bob');
    expect(notify.success).toHaveBeenCalledWith('Invitation revoked');
    expect(root.querySelector('button[aria-label="Revoke invitation for bob"]')).toBeNull();
    expect(revokeButton('erin')).not.toBeNull();
  });

  it('keeps the invitation when the revocation is cancelled', () => {
    const { fixture, requests, revokeButton } = setup('project', 'cancel');
    revokeButton('bob').click();
    fixture.detectChanges();
    expect(requests.revokeInvitation).not.toHaveBeenCalled();
    expect(revokeButton('bob')).not.toBeNull();
  });

  it('lists team requests below invitations on projects and caches but not on teams', () => {
    expect(setup('project').root.textContent).toContain('Team Requests');
    TestBed.resetTestingModule();
    expect(setup('team').root.textContent).not.toContain('Team Requests');
  });
});
