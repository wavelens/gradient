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
import { MembersRolesComponent } from './members-roles.component';
import { ProjectsService } from '@core/services/projects.service';
import { UserService } from '@core/services/user.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { AccessState } from '@core/models/access.model';

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

function setup(access: AccessState): ComponentFixture<MembersRolesComponent> {
  revokeInvitation.mockClear();
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
          getMembers: () => of([{ id: 'alice', name: 'Admin' }]),
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
        },
      },
      { provide: UserService, useValue: { listUsers: () => of([]) } },
      { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve(access) } },
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
});
