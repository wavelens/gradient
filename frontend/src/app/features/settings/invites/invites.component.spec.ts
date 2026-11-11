/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { RequestsPageComponent } from './invites.component';
import { UserService } from '@core/services/user.service';
import { CachesService } from '@core/services/caches.service';
import { TeamsService } from '@core/services/teams.service';
import { NotifyService } from '@core/services/notify.service';
import { AuthService } from '@core/services/auth.service';
import { InviteItem, PendingUserDecisionsResponse, SubscriptionRequestItem, TeamGrantRequestInboxEntry } from '@core/models';
import { PendingUserDecisionsService } from '@core/services/pending-invites.service';

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

const subscriptionRequest: SubscriptionRequestItem = {
  cache: 'main',
  cache_display_name: 'Main Cache',
  project: 'shop',
  project_display_name: 'Web Shop',
  mode: 'ReadOnly',
  requested_by: 'dave',
  requested_by_display_name: 'Dave',
  created_at: '2026-09-05T12:00:00',
};

const teamRequest: TeamGrantRequestInboxEntry = {
  id: 'req-1',
  kind: 'project',
  target: 'docs',
  display_name: 'Docs Site',
  role: 'Write',
  users: true,
  workers: true,
  requested_by: 'erin',
  created_at: '2026-09-05T12:00:00',
  team: 'infra',
  team_display_name: 'Infra Team',
};

function decisions(partial: Partial<PendingUserDecisionsResponse>): PendingUserDecisionsResponse {
  return { invites: [], subscription_requests: [], team_requests: [], ...partial };
}

function setup(
  invites: InviteItem[],
  queryToken: string | null = null,
  getRequests: () => Observable<PendingUserDecisionsResponse> = () => of(decisions({ invites })),
) {
  const accept = vi.fn().mockReturnValue(of('Invitation accepted'));
  const decline = vi.fn().mockReturnValue(of('Invitation declined'));
  const approveSubscriptionRequest = vi.fn().mockReturnValue(of('ok'));
  const denySubscriptionRequest = vi.fn().mockReturnValue(of('ok'));
  const approveRequest = vi.fn().mockReturnValue(of('ok'));
  const denyRequest = vi.fn().mockReturnValue(of('ok'));
  const notify = { success: vi.fn(), failure: vi.fn() };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());

  TestBed.configureTestingModule({
    imports: [RequestsPageComponent],
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
      { provide: AuthService, useValue: { isAuthenticated: () => false } },
      {
        provide: UserService,
        useValue: {
          getRequestsAwaitingUserDecision: getRequests,
          acceptInvite: accept,
          declineInvite: decline,
        },
      },
      { provide: CachesService, useValue: { approveSubscriptionRequest, denySubscriptionRequest } },
      { provide: TeamsService, useValue: { approveRequest, denyRequest } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });

  const fixture: ComponentFixture<RequestsPageComponent> = TestBed.createComponent(RequestsPageComponent);
  fixture.detectChanges();
  return { fixture, accept, decline, notify, confirm, approveSubscriptionRequest, denySubscriptionRequest, approveRequest, denyRequest };
}

function buttonByText(root: HTMLElement, text: string): HTMLButtonElement | undefined {
  return Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent || '').trim().includes(text),
  ) as HTMLButtonElement | undefined;
}

function sectionButton(root: HTMLElement, section: string, text: string): HTMLButtonElement {
  return buttonByText(root.querySelector<HTMLElement>(section)!, text)!;
}

function allRequests() {
  return () => of(decisions({ invites: [projectInvite], subscription_requests: [subscriptionRequest], team_requests: [teamRequest] }));
}

describe('RequestsPageComponent - loading', () => {
  it('shows a failed load with Try Again instead of No pending requests', () => {
    const getRequests = vi.fn().mockReturnValueOnce(throwError(() => new Error('database is down'))).mockReturnValue(of(decisions({ invites: [projectInvite] })));
    const { fixture } = setup([], null, getRequests);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load requests');
    expect(root.textContent).not.toContain('No pending requests');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getRequests).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('Wavelens');
  });
});

describe('RequestsPageComponent - requests', () => {
  it('lists invitations, cache subscription requests and team grant requests in their own sections', () => {
    const { fixture } = setup([], null, allRequests());
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.invites')?.textContent).toContain('Wavelens');
    const subscription = root.querySelector('.subscription-requests')!;
    expect(subscription.textContent).toContain('Web Shop');
    expect(subscription.textContent).toContain('Main Cache');
    expect(subscription.textContent).toContain('Read Only');
    expect(subscription.textContent).toContain('Requested by Dave');
    const team = root.querySelector('.team-requests')!;
    expect(team.textContent).toContain('Docs Site');
    expect(team.textContent).toContain('Infra Team');
    expect(team.textContent).toContain('Members get Write');
    expect(team.textContent).toContain('Team workers');
  });

  it('links each request to its project, cache and team', () => {
    const { fixture } = setup([], null, allRequests());
    const root = fixture.nativeElement as HTMLElement;
    const hrefs = (section: string) => Array.from(root.querySelectorAll(`${section} gr-row a`)).map((a) => a.getAttribute('href'));
    expect(hrefs('.subscription-requests')).toEqual(['/project/shop', '/caches/main']);
    expect(hrefs('.team-requests')).toEqual(['/project/docs', '/team/infra']);
  });

  it('approves a subscription request through the cache endpoint and removes its row', () => {
    const { fixture, approveSubscriptionRequest, notify } = setup([], null, allRequests());
    const root = fixture.nativeElement as HTMLElement;
    sectionButton(root, '.subscription-requests', 'Approve').click();
    fixture.detectChanges();
    expect(approveSubscriptionRequest).toHaveBeenCalledWith('main', 'shop');
    expect(notify.success).toHaveBeenCalledWith('Subscription request approved');
    expect(root.querySelector('.subscription-requests')).toBeNull();
    expect(TestBed.inject(PendingUserDecisionsService).count()).toBe(2);
  });

  it('asks before denying a subscription request, then denies it and removes its row', () => {
    const { fixture, denySubscriptionRequest, confirm } = setup([], null, allRequests());
    const root = fixture.nativeElement as HTMLElement;
    sectionButton(root, '.subscription-requests', 'Deny').click();
    fixture.detectChanges();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Web Shop (shop) asks to use Main Cache (main)' }));
    expect(denySubscriptionRequest).toHaveBeenCalledWith('main', 'shop');
    expect(root.querySelector('.subscription-requests')).toBeNull();
  });

  it('keeps the subscription request when denying is cancelled', () => {
    const { fixture, denySubscriptionRequest, confirm } = setup([], null, allRequests());
    confirm.mockImplementation(() => EMPTY);
    sectionButton(fixture.nativeElement, '.subscription-requests', 'Deny').click();
    expect(denySubscriptionRequest).not.toHaveBeenCalled();
  });

  it('approves a team grant request through the team endpoint and removes its row', () => {
    const { fixture, approveRequest } = setup([], null, allRequests());
    const root = fixture.nativeElement as HTMLElement;
    sectionButton(root, '.team-requests', 'Approve').click();
    fixture.detectChanges();
    expect(approveRequest).toHaveBeenCalledWith('infra', 'req-1');
    expect(root.querySelector('.team-requests')).toBeNull();
  });

  it('asks before denying a team grant request, then denies it and removes its row', () => {
    const { fixture, denyRequest, confirm } = setup([], null, allRequests());
    const root = fixture.nativeElement as HTMLElement;
    sectionButton(root, '.team-requests', 'Deny').click();
    fixture.detectChanges();
    expect(confirm).toHaveBeenCalled();
    expect(denyRequest).toHaveBeenCalledWith('infra', 'req-1');
    expect(root.querySelector('.team-requests')).toBeNull();
  });

  it('reports a failed approval and keeps the row', () => {
    const { fixture, approveRequest, notify } = setup([], null, allRequests());
    approveRequest.mockReturnValue(throwError(() => new Error('refused')));
    const root = fixture.nativeElement as HTMLElement;
    sectionButton(root, '.team-requests', 'Approve').click();
    fixture.detectChanges();
    expect(notify.failure).toHaveBeenCalled();
    expect(root.querySelector('.team-requests')).not.toBeNull();
  });
});

describe('RequestsPageComponent - invitations', () => {
  it('opens what the user joined right from the accepted invite', () => {
    const { fixture } = setup([projectInvite]);
    const root = fixture.nativeElement as HTMLElement;
    buttonByText(root, 'Accept')?.click();
    fixture.detectChanges();
    const open = root.querySelector<HTMLAnchorElement>('.joined a[grButton]');
    expect(open?.textContent).toContain('Open project');
    expect(open?.getAttribute('href')).toBe('/project/wavelens');
  });

  it('shows the joined row without an empty state once the last invite is accepted', () => {
    const { fixture } = setup([projectInvite]);
    const root = fixture.nativeElement as HTMLElement;
    buttonByText(root, 'Accept')?.click();
    fixture.detectChanges();
    expect(root.querySelector('.joined')?.textContent).toContain('Wavelens');
    expect(root.querySelector('.invites')).toBeNull();
    expect(root.textContent).not.toContain('No pending requests');
  });

  it('labels the team role sent as its enum value', () => {
    const { fixture } = setup([{ ...teamInvite, role: 'member' }]);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Role Member');
  });

  it('shares the loaded requests with the header menu and the dashboard banner', () => {
    setup([], null, allRequests());
    expect(TestBed.inject(PendingUserDecisionsService).count()).toBe(3);
  });

  it('lists a pending invite with its scope and role', () => {
    const { fixture } = setup([projectInvite]);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Wavelens');
    expect(text).toContain('Admin');
  });

  it('shows an empty state when nothing waits for a decision', () => {
    const { fixture } = setup([]);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('No pending requests');
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

  it('accepts the token from the invitation email after loading and shows what the user joined', () => {
    const navigate = vi.spyOn(Router.prototype, 'navigateByUrl').mockResolvedValue(true);
    const { accept, fixture } = setup([projectInvite], 'tok-project');
    fixture.detectChanges();
    expect(accept).toHaveBeenCalledWith('tok-project');
    expect(String(navigate.mock.calls.at(-1)?.[0])).toBe('/settings/invites');
    expect(navigate.mock.calls.at(-1)?.[1]).toEqual({ replaceUrl: true });
    expect((fixture.nativeElement as HTMLElement).querySelector('.joined')?.textContent).toContain('Wavelens');
  });

  it('still accepts the token from the invitation email when the list fails to load', () => {
    vi.spyOn(Router.prototype, 'navigateByUrl').mockResolvedValue(true);
    const { accept } = setup([], 'tok-project', () => throwError(() => new Error('down')));
    expect(accept).toHaveBeenCalledWith('tok-project');
  });
});
