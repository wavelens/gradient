/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { InvitesComponent } from './invites.component';
import { UserService } from '@core/services/user.service';
import { Invite } from '@core/models';

const projectInvite: Invite = {
  kind: 'project',
  token: 'tok-project',
  scope: 'wavelens',
  scope_display_name: 'Wavelens',
  role: 'Admin',
  invited_by: 'alice',
  created_at: '2026-09-05T12:00:00',
  expires_at: '2026-09-12T12:00:00',
};

function setup(invites: Invite[], queryToken: string | null = null) {
  const accept = vi.fn().mockReturnValue(of('Invitation accepted'));
  const decline = vi.fn().mockReturnValue(of('Invitation declined'));

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
          getInvites: () => of(invites),
          acceptInvite: accept,
          declineInvite: decline,
        },
      },
    ],
  });

  const fixture: ComponentFixture<InvitesComponent> = TestBed.createComponent(InvitesComponent);
  fixture.detectChanges();
  return { fixture, accept, decline };
}

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent || '').trim().includes(text),
  ) as HTMLButtonElement | undefined;
}

describe('InvitesComponent', () => {
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

  it('declines an invite by token when Decline is clicked', () => {
    const { fixture, decline } = setup([projectInvite]);
    buttonByText(fixture.nativeElement, 'Decline')?.click();
    expect(decline).toHaveBeenCalledWith('tok-project');
  });

  it('accepts the token carried in the query string on load', () => {
    const { accept } = setup([projectInvite], 'tok-project');
    expect(accept).toHaveBeenCalledWith('tok-project');
  });
});
