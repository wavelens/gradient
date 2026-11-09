/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { RoleResponse_ProjectRoleOwner } from '@core/models';
import { MembersRolesComponent } from './members-roles.component';
import { ProjectsService } from '@core/services/projects.service';
import { UserService } from '@core/services/user.service';
import { projectRouteStub } from '@core/resolvers/access-route.testing';
import { AccessState } from '@core/models/access.model';
import type { Permission } from '@core/models';
import { NotifyService } from '@core/services/notify.service';


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
const deleteRole = vi.fn();
const notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function setup(access: AccessState, permissions?: Permission[], role?: string | null): ComponentFixture<MembersRolesComponent> {
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  deleteRole.mockReset().mockReturnValue(of(true));
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
      {
        provide: ActivatedRoute,
        useFactory: () => projectRouteStub('acme', access, { ...(permissions && { permissions }), ...(role !== undefined && { role }) }),
      },
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
          deleteRole,
        },
      },
      { provide: ConfirmService, useValue: { confirm } },
      { provide: UserService, useValue: { listUsers: () => of([]) } },
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
  it('hides Invite Member, New Role, per-row Remove under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'invite member')).toBeNull();
    expect(findByText(fixture.nativeElement, 'new role')).toBeNull();
  });

  it('shows but disables Invite Member / New Role under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const addBtn = findByText(fixture.nativeElement, 'invite member') as HTMLButtonElement | null;
    const newRoleBtn = findByText(fixture.nativeElement, 'new role') as HTMLButtonElement | null;
    expect(addBtn).not.toBeNull();
    expect(addBtn!.disabled).toBe(true);
    expect(newRoleBtn).not.toBeNull();
    expect(newRoleBtn!.disabled).toBe(true);
  });

  it('lists the members to a visitor outside the project but shows the roles as no access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [], null);
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(getMembers).toHaveBeenCalled();
    expect(root.textContent).toContain('No access');
    expect(root.textContent).toContain('Only the members of this project can view this.');
    expect(root.textContent).not.toContain('Failed to load roles');
  });

  it('names the role of each member as text instead of a select under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false }, [], null);
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-row gr-select')).toBeNull();
    expect(root.querySelector('.member-role')?.textContent?.trim()).toBe('Admin');
  });

  it('names each member by username and adds the display name when it differs', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    getMembers.mockReturnValue(of([
      { id: 'alice', user_display_name: 'Alice Doe', name: 'Admin' },
      { id: 'bob', user_display_name: 'bob', name: 'Admin' },
    ]));
    await settled(fixture);
    const rows = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-row'));
    expect(rows[0].querySelector('.mono')?.textContent?.trim()).toBe('alice');
    expect(rows[0].querySelector('[slot="meta"]')?.textContent?.trim()).toBe('Alice Doe');
    expect(rows[1].querySelector('[slot="meta"]')).toBeNull();
  });

  it('offers the role select to members who can manage members', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('gr-row gr-select')).not.toBeNull();
    expect(root.querySelector('.member-role')).toBeNull();
  });
});

describe('MembersRolesComponent - permissions', () => {
  const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('offers member writes but no role writes without the manage roles permission', async () => {
    const fixture = setup(writable, ['viewProject', 'manageMembers']);
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'invite member')).not.toBeNull();
    expect(findByText(fixture.nativeElement, 'new role')).toBeNull();
  });

  it('offers role writes but neither member writes nor invitations without the manage members permission', async () => {
    const fixture = setup(writable, ['viewProject', 'manageRoles']);
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(findByText(fixture.nativeElement, 'invite member')).toBeNull();
    expect(findByText(fixture.nativeElement, 'new role')).not.toBeNull();
    expect(text).not.toContain('Pending Invitations');
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

  it('asks before revoking a pending invitation, naming the invited user', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'bob' }));
    expect(revokeInvitation).toHaveBeenCalledWith('acme', 'bob');
    expect(notify.success).toHaveBeenCalledWith('Invitation revoked');
  });

  it('sends no revoke when the confirmation is cancelled', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(revokeInvitation).not.toHaveBeenCalled();
  });
});

describe('MembersRolesComponent - writes', () => {
  it('asks before removing a member, naming the member', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('gr-row-list button.gr-button--danger')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Remove member', target: 'alice' }));
    expect(removeMember).toHaveBeenCalledWith('acme', 'alice');
    expect(notify.success).toHaveBeenCalledWith('Member removed');
  });

  it('sends no member removal when the confirmation is cancelled', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    fixture.componentInstance.removeMember('alice');
    expect(removeMember).not.toHaveBeenCalled();
  });

  it('asks before deleting a role, naming the role', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    fixture.componentInstance.deleteRole({ id: 'r2', name: 'releaser', builtin: false } as RoleResponse_ProjectRoleOwner);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete role', target: 'releaser' }));
    expect(deleteRole).toHaveBeenCalledWith('acme', 'r2');
  });

  it('keeps a failed add inside the open Invite Member dialog', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    addMember.mockReturnValue(throwError(() => new ApiError('User carol does not exist', 404)));
    await settled(fixture);
    (findByText(fixture.nativeElement, 'invite member') as HTMLButtonElement).click();
    fixture.detectChanges();
    fixture.componentInstance.newMember = { user: 'carol', role: 'Admin' };
    fixture.componentInstance.addMember();
    fixture.detectChanges();
    const dialog = document.body.querySelector('.gr-dialog')!;
    expect(dialog.querySelector('gr-message-banner')?.textContent).toContain('User carol does not exist');
    expect(fixture.componentInstance.showAddMemberDialog()).toBe(true);
  });

  it('sends one add and keeps the Invite Member dialog open while it runs', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    addMember.mockReturnValue(new Subject<string>());
    await settled(fixture);
    (findByText(fixture.nativeElement, 'invite member') as HTMLButtonElement).click();
    fixture.detectChanges();
    fixture.componentInstance.newMember = { user: 'carol', role: 'Admin' };
    fixture.componentInstance.addMember();
    fixture.detectChanges();
    fixture.componentInstance.addMember();
    const dialog = document.body.querySelector('.gr-dialog')!;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    fixture.detectChanges();
    expect(addMember).toHaveBeenCalledTimes(1);
    expect(fixture.componentInstance.showAddMemberDialog()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });

  it('reports an invitation with the server message and lists it at once', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    addMember.mockReturnValue(of('Invitation sent'));
    await settled(fixture);
    const loadInvitations = vi.spyOn(fixture.componentInstance, 'loadInvitations');
    fixture.componentInstance.newMember = { user: 'dave', role: 'Admin' };
    fixture.componentInstance.addMember();
    expect(notify.success).toHaveBeenCalledWith('Invitation sent');
    expect(loadInvitations).toHaveBeenCalled();
  });

  it('preselects no role, so adding a member needs a deliberate role choice', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    await settled(fixture);
    fixture.componentInstance.openAddMemberDialog();
    fixture.componentInstance.newMember.user = 'dave';
    fixture.detectChanges();

    expect(fixture.componentInstance.newMember.role).toBe('');
    const add = Array.from(document.querySelectorAll<HTMLButtonElement>('.gr-dialog button')).find((b) => b.textContent?.trim() === 'Invite');
    expect(add?.disabled).toBe(true);
  });
});

describe('MembersRolesComponent - role form', () => {
  const custom = { id: 'r2', name: 'releaser', builtin: false, permissions: ['trigger'], project: 'o' } as unknown as RoleResponse_ProjectRoleOwner;

  async function withRoleWrites() {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    const writes = { createRole: vi.fn(() => of(custom)), updateRole: vi.fn(() => of(custom)) };
    Object.assign(TestBed.inject(ProjectsService), writes);
    await settled(fixture);
    return { fixture, cmp: fixture.componentInstance, writes };
  }

  const fieldError = () => document.querySelector('.gr-dialog .field-error')?.textContent?.trim() ?? '';

  it('refuses a role named like a built-in role in another letter case', async () => {
    const { fixture, cmp, writes } = await withRoleWrites();
    cmp.openCreateRoleDialog();
    cmp.roleForm.name = 'admin';
    cmp.saveRole();
    fixture.detectChanges();
    expect(fieldError()).toContain('already exist');
    expect(writes.createRole).not.toHaveBeenCalled();
  });

  it('refuses a role name with uppercase letters or spaces', async () => {
    const { fixture, cmp, writes } = await withRoleWrites();
    cmp.openCreateRoleDialog();
    cmp.roleForm.name = 'Bad Name';
    cmp.saveRole();
    fixture.detectChanges();
    expect(fieldError()).toContain('lowercase');
    expect(writes.createRole).not.toHaveBeenCalled();
  });

  it('closes the edit dialog without a request when nothing changed', async () => {
    const { cmp, writes } = await withRoleWrites();
    cmp.roles.set([...cmp.roles(), custom]);
    cmp.openEditRoleDialog(custom);
    cmp.roleForm.permissions = { trigger: true };
    cmp.saveRole();
    expect(writes.updateRole).not.toHaveBeenCalled();
    expect(cmp.showRoleDialog()).toBe(false);
  });

  it('sends only the changed role name', async () => {
    const { cmp, writes } = await withRoleWrites();
    cmp.roles.set([...cmp.roles(), custom]);
    cmp.openEditRoleDialog(custom);
    cmp.roleForm.permissions = { trigger: true };
    cmp.roleForm.name = 'deployer';
    cmp.saveRole();
    expect(writes.updateRole).toHaveBeenCalledWith('acme', 'r2', { name: 'deployer' });
  });
});

describe('MembersRolesComponent - load errors', () => {
  it('shows a failed member load with Try Again instead of No members', async () => {
    const fixture = setup({ managed: false, canEdit: true, canTrigger: true });
    getMembers.mockReturnValueOnce(throwError(() => new ApiError('Database is down', 500)));
    await settled(fixture);
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('Failed to load members');
    expect(element.textContent).toContain('Database is down');
    expect(element.textContent).not.toContain('No members');
    element.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    await settled(fixture);
    expect(getMembers).toHaveBeenCalledTimes(2);
    expect(element.textContent).toContain('alice');
  });
});
