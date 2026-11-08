/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { cacheAccessData } from '@core/resolvers/access-route.testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Subject, of, throwError } from 'rxjs';
import { ApiError } from '@core/services/api.service';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { CacheMembersRolesComponent } from './cache-members-roles.component';
import { CachesService } from '@core/services/caches.service';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { CachePermission, RoleResponse_CacheRoleOwner } from '@core/models';

function activatedRouteStub(canEdit = false, permissions?: CachePermission[]) {
  const data = cacheAccessData({ managed: false, canEdit, canTrigger: canEdit });
  if (permissions) data.cacheAccess.cache.permissions = permissions;
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'my-cache' }) },
    parent: { data: of(data) },
  } as unknown as ActivatedRoute;
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
const deleteRole = vi.fn();
const notify = { success: vi.fn(), failure: vi.fn() };
const confirm = vi.fn();

function setup(canEdit = false, permissions?: CachePermission[]): ComponentFixture<CacheMembersRolesComponent> {
  confirm.mockReset().mockImplementation((request: ConfirmRequest) => request.action());
  revokeInvitation.mockReset().mockReturnValue(of('Invitation revoked'));
  getMembers.mockReset().mockReturnValue(of([{ id: 'alice', name: 'Admin' }]));
  removeMember.mockReset().mockReturnValue(of('ok'));
  deleteRole.mockReset().mockReturnValue(of(true));
  notify.success.mockClear();
  notify.failure.mockClear();
  TestBed.configureTestingModule({
    imports: [CacheMembersRolesComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub(canEdit, permissions) },
      {
        provide: CachesService,
        useValue: {
          getCache: () => of({ display_name: 'Prod' }),
          getMembers,
          getRoles: () =>
            of({
              roles: [{ id: 'r1', name: 'Admin', builtin: true, managed: false, permissions: [], cache: null }],
              available_permissions: [],
            }),
          addMember: () => of('ok'),
          updateMember: () => of('ok'),
          removeMember,
          createRole: () => of({}),
          updateRole: () => of({}),
          deleteRole,
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
        },
      },
      { provide: UserService, useValue: { searchUsers: () => of([]) } },
      { provide: NotifyService, useValue: notify },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  return TestBed.createComponent(CacheMembersRolesComponent);
}

async function settled(fixture: ComponentFixture<CacheMembersRolesComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('CacheMembersRolesComponent', () => {
  it('renders without errors and shows members section', async () => {
    const fixture = setup();
    await settled(fixture);
    expect(fixture.nativeElement.textContent).toContain('Members');
  });

  it('shows a failed member load with Try Again instead of No members', async () => {
    const fixture = setup();
    getMembers.mockReturnValueOnce(throwError(() => new ApiError('database is down', 500)));
    await settled(fixture);
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('Failed to load members');
    expect(element.textContent).not.toContain('No members');
    element.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    await settled(fixture);
    expect(getMembers).toHaveBeenCalledTimes(2);
    expect(element.textContent).toContain('alice');
  });

  it('hides Add Member and New Role buttons under read-only access (no writable)', async () => {
    const fixture = setup();
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'add member')).toBeNull();
    expect(findByText(fixture.nativeElement, 'new role')).toBeNull();
  });

  it('shows members and roles as no access to a visitor without a cache role and asks the server nothing', async () => {
    const fixture = setup(false, []);
    await settled(fixture);
    const element = fixture.nativeElement as HTMLElement;
    expect(getMembers).not.toHaveBeenCalled();
    expect(element.textContent).toContain('Only the members of this cache can view this.');
    expect(element.textContent).not.toContain('Failed to load');
    expect(element.textContent).not.toContain('Try Again');
  });
});

describe('CacheMembersRolesComponent - pending invitations', () => {
  it('lists a pending invitation separately from members', async () => {
    const fixture = setup(true);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Pending Invitations');
    expect(text).toContain('bob');
  });

  it('asks before revoking a pending invitation, naming the invited user', async () => {
    const fixture = setup(true);
    await settled(fixture);
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'bob' }));
    expect(revokeInvitation).toHaveBeenCalledWith('my-cache', 'bob');
    expect(notify.success).toHaveBeenCalledWith('Invitation revoked');
  });

  it('sends no revoke when the confirmation is cancelled', async () => {
    const fixture = setup(true);
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(revokeInvitation).not.toHaveBeenCalled();
    expect(notify.success).not.toHaveBeenCalled();
  });
});

describe('CacheMembersRolesComponent - busy dialogs', () => {
  it('keeps Add Member open and sends one request while adding', async () => {
    const fixture = setup(true);
    const service = TestBed.inject(CachesService) as unknown as { addMember: () => unknown };
    service.addMember = vi.fn(() => new Subject<string>());
    await settled(fixture);
    const component = fixture.componentInstance;
    component.openAddMemberDialog();
    component.newMember = { user: 'carol', role: 'Admin' };
    await settled(fixture);
    component.addMember();
    component.addMember();
    await settled(fixture);
    const dialog = document.querySelector('.gr-dialog') as HTMLElement;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    await settled(fixture);
    expect(service.addMember).toHaveBeenCalledTimes(1);
    expect(component.showAddMemberDialog()).toBe(true);
    expect(dialog.getAttribute('aria-busy')).toBe('true');
  });
});

describe('CacheMembersRolesComponent - confirmed removals', () => {
  it('asks before removing a member, naming the member', async () => {
    const fixture = setup(true);
    await settled(fixture);
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('gr-row-list button.gr-button--danger')!.click();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Remove member', target: 'alice' }));
    expect(removeMember).toHaveBeenCalledWith('my-cache', 'alice');
    expect(notify.success).toHaveBeenCalledWith('Member removed');
  });

  it('sends no member removal when the confirmation is cancelled', async () => {
    const fixture = setup(true);
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    fixture.componentInstance.removeMember('alice');
    expect(removeMember).not.toHaveBeenCalled();
  });

  it('asks before deleting a role, naming the role', async () => {
    const fixture = setup(true);
    await settled(fixture);
    fixture.componentInstance.deleteRole({ id: 'r2', name: 'releaser', builtin: false } as RoleResponse_CacheRoleOwner);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete role', target: 'releaser' }));
    expect(deleteRole).toHaveBeenCalledWith('my-cache', 'r2');
    expect(notify.success).toHaveBeenCalledWith('Role deleted');
  });
});

describe('CacheMembersRolesComponent - role change', () => {
  it('Confirm the role of the saved member with a toast', async () => {
    const fixture = setup(true);
    await settled(fixture);
    fixture.componentInstance.updateMemberRole('alice', 'Admin');
    expect(notify.success).toHaveBeenCalledWith('Member role saved');
  });
});

describe('CacheMembersRolesComponent - busy rows', () => {
  it('Disable role dropdowns and Remove buttons on all member rows during a save of one role change.', async () => {
    const fixture = setup(true);
    getMembers.mockReturnValue(of([{ id: 'alice', name: 'Admin' }, { id: 'dave', name: 'Admin' }]));
    const service = TestBed.inject(CachesService) as unknown as { updateMember: () => unknown };
    service.updateMember = vi.fn(() => new Subject());
    await settled(fixture);
    fixture.componentInstance.updateMemberRole('dave', 'Admin');
    await settled(fixture);
    const controls = Array.from((fixture.nativeElement as HTMLElement).querySelector('gr-row-list')!.querySelectorAll('button'));
    expect(controls.length).toBe(4);
    expect(controls.every((control) => control.disabled)).toBe(true);
  });

  it('Keep the role button disabled until role deletion is complete.', async () => {
    const fixture = setup(true);
    const pending = new Subject<boolean>();
    deleteRole.mockReturnValue(pending);
    await settled(fixture);
    fixture.componentInstance.deleteRole({ id: 'r2', name: 'releaser', builtin: false } as RoleResponse_CacheRoleOwner);
    expect(fixture.componentInstance.deletingRole()).toBe(true);
    pending.next(true);
    pending.complete();
    expect(fixture.componentInstance.deletingRole()).toBe(false);
  });
});
