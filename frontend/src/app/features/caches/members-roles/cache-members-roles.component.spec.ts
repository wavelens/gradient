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
import { CacheMembersRolesComponent } from './cache-members-roles.component';
import { CachesService } from '@core/services/caches.service';
import { UserService } from '@core/services/user.service';

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

function setup(canEdit = false): ComponentFixture<CacheMembersRolesComponent> {
  revokeInvitation.mockClear();
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
          getMembers: () => of([{ id: 'alice', name: 'Admin' }]),
          getRoles: () =>
            of({
              roles: [{ id: 'r1', name: 'Admin', builtin: true, managed: false, permissions: [], cache: null }],
              available_permissions: [],
            }),
          addMember: () => of('ok'),
          updateMember: () => of('ok'),
          removeMember: () => of('ok'),
          createRole: () => of({}),
          updateRole: () => of({}),
          deleteRole: () => of(true),
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
});
