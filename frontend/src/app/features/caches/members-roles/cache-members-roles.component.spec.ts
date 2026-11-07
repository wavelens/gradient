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
import { CacheMembersRolesComponent } from './cache-members-roles.component';
import { CachesService } from '@core/services/caches.service';
import { UserService } from '@core/services/user.service';
import { NotifyService } from '@core/services/notify.service';
import { RoleResponse_CacheRoleOwner } from '@core/models';

function activatedRouteStub(canEdit = false) {
  const data = canEdit
    ? { cacheAccess: { access: { managed: false, canEdit: true, canTrigger: true } } }
    : {};
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

function setup(canEdit = false): ComponentFixture<CacheMembersRolesComponent> {
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
      { provide: ActivatedRoute, useValue: activatedRouteStub(canEdit) },
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
});

describe('CacheMembersRolesComponent - pending invitations', () => {
  it('lists a pending invitation separately from members', async () => {
    const fixture = setup();
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent || '';
    expect(text).toContain('Pending Invitations');
    expect(text).toContain('bob');
  });

  it('revokes a pending invitation', async () => {
    const fixture = setup(true);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(revokeInvitation).toHaveBeenCalledWith('my-cache', 'bob');
  });

  it('reports a failed revoke as a toast, not inside the closed Add Member dialog', async () => {
    const fixture = setup(true);
    const failure = new ApiError('The server failed to process the request. Try again later.', 500);
    revokeInvitation.mockReturnValue(throwError(() => failure));
    await settled(fixture);
    (findByText(fixture.nativeElement, 'revoke') as HTMLButtonElement).click();
    expect(notify.failure).toHaveBeenCalledWith('Failed to revoke invitation', failure);
    expect(fixture.componentInstance.memberError()).toBeNull();
  });
});

describe('CacheMembersRolesComponent - write failures', () => {
  it('reports a failed member removal', async () => {
    const fixture = setup(true);
    const failure = new ApiError('The server failed to process the request. Try again later.', 500);
    removeMember.mockReturnValue(throwError(() => failure));
    await settled(fixture);
    fixture.componentInstance.removeMember('alice');
    expect(notify.failure).toHaveBeenCalledWith('Failed to remove member', failure);
    expect(fixture.componentInstance.removingMember()).toBeNull();
  });

  it('reports a failed role delete', async () => {
    const fixture = setup(true);
    const failure = new ApiError('Role is still assigned', 409);
    deleteRole.mockReturnValue(throwError(() => failure));
    await settled(fixture);
    fixture.componentInstance.deleteRole({ id: 'r2', name: 'releaser', builtin: false } as RoleResponse_CacheRoleOwner);
    expect(notify.failure).toHaveBeenCalledWith('Failed to delete role', failure);
    expect(fixture.componentInstance.deletingRole()).toBeNull();
  });
});
