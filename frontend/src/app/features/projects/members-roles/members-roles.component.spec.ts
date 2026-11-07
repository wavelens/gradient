/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { MembersRolesComponent } from './members-roles.component';
import { ProjectsService } from '@core/services/projects.service';
import { UserService } from '@core/services/user.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { AccessState } from '@core/models/access.model';
import { NotifyService } from '@core/services/notify.service';

function activatedRouteStub() {
  return { snapshot: { paramMap: convertToParamMap({ project: 'acme' }) } } as Partial<ActivatedRoute>;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

const revokeInvitation = vi.fn().mockReturnValue(of('Invitation revoked'));
const getMembers = vi.fn();
const removeMember = vi.fn();
const addMember = vi.fn();
const notify = { success: vi.fn(), failure: vi.fn() };

function setup(access: AccessState): ComponentFixture<MembersRolesComponent> {
  revokeInvitation.mockReset().mockReturnValue(of('Invitation revoked'));
  getMembers.mockReset().mockReturnValue(of([{ id: 'alice', name: 'Admin' }]));
  removeMember.mockReset().mockReturnValue(of('ok'));
  addMember.mockReset().mockReturnValue(of('ok'));
  notify.success.mockClear();
  notify.failure.mockClear();
  TestBed.configureTestingModule({
    imports: [MembersRolesComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub() },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () => of({ display_name: 'Acme' }),
          getMembers,
          getRoles: () =>
            of({
              roles: [{ id: 'r1', name: 'Admin', builtin: true, permissions: [], project: null }],
              available_permissions: [],
            }),
          getInvitations: () =>
            of([
              {
                user: 'bob',
                name: 'Bob',
                role: 'Admin',
                created_at: '2026-09-05T12:00:00',
                expires_at: '2026-09-12T12:00:00',
              },
            ]),
          revokeInvitation,
          removeMember,
          addMember,
        },
      },
      { provide: UserService, useValue: { listUsers: () => of([]) } },
      { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve(access) } },
      { provide: NotifyService, useValue: notify },
    ],
  });
  return TestBed.createComponent(MembersRolesComponent);
}

async function settled(fixture: ComponentFixture<MembersRolesComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('MembersRolesComponent - access gating', () => {
  it('hides Add Member, New Role, per-row Remove under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'add member')).toBeNull();
    expect(findByText(fixture.nativeElement, 'new role')).toBeNull();
  });

  it('shows but disables Add Member / New Role under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const addBtn = findByText(fixture.nativeElement, 'add member') as HTMLButtonElement | null;
    const newRoleBtn = findByText(fixture.nativeElement, 'new role') as HTMLButtonElement | null;
    expect(addBtn).not.toBeNull();
    expect(addBtn!.disabled).toBe(true);
    expect(newRoleBtn).not.toBeNull();
    expect(newRoleBtn!.disabled).toBe(true);
  });
});

describe('MembersRolesComponent - pending invitations', () => {
  it('lists a pending invitation separately from members', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Pending Invitations');
    expect(text).toContain('bob');
  });

  it('revokes a pending invitation', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(revokeInvitation).toHaveBeenCalledWith('acme', 'bob');
  });

  it('reports a failed revoke as a toast, not inside the closed Add Member dialog', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const failure = new ApiError('The server failed to process the request. Try again later.', 500);
    revokeInvitation.mockReturnValue(throwError(() => failure));
    await settled(fixture);
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(notify.failure).toHaveBeenCalledWith('Failed to revoke invitation', failure);
    expect(fixture.componentInstance.memberError()).toBeNull();
    expect(fixture.componentInstance.revokingUser()).toBeNull();
  });
});

describe('MembersRolesComponent - write failures', () => {
  it('reports a failed member removal', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const failure = new ApiError('The server failed to process the request. Try again later.', 500);
    removeMember.mockReturnValue(throwError(() => failure));
    await settled(fixture);
    fixture.componentInstance.removeMember('alice');
    expect(notify.failure).toHaveBeenCalledWith('Failed to remove member', failure);
    expect(fixture.componentInstance.removingMember()).toBeNull();
  });

  it('keeps a failed add inside the open Add Member dialog', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    addMember.mockReturnValue(throwError(() => new ApiError('User carol does not exist', 404)));
    await settled(fixture);
    (findByText(fixture.nativeElement, 'add member') as HTMLButtonElement).click();
    fixture.detectChanges();
    fixture.componentInstance.newMember.user = 'carol';
    fixture.componentInstance.addMember();
    fixture.detectChanges();
    const dialog = document.body.querySelector('.gr-dialog')!;
    expect(dialog.querySelector('gr-message-banner')?.textContent).toContain('User carol does not exist');
    expect(fixture.componentInstance.showAddMemberDialog()).toBe(true);
  });
});

describe('MembersRolesComponent - load errors', () => {
  it('shows a failed member load with Try Again instead of No members', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    getMembers.mockReturnValueOnce(throwError(() => new ApiError('Forbidden', 403)));
    await settled(fixture);
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('Failed to load members');
    expect(element.textContent).toContain('No permission');
    expect(element.textContent).not.toContain('No members');
    element.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    await settled(fixture);
    expect(getMembers).toHaveBeenCalledTimes(2);
    expect(element.textContent).toContain('alice');
  });
});
