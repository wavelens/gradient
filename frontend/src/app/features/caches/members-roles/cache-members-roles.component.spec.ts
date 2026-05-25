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

function activatedRouteStub() {
  return {
    snapshot: { paramMap: convertToParamMap({ cache: 'my-cache' }) },
    parent: { data: of({}) },
  } as unknown as ActivatedRoute;
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function setup(): ComponentFixture<CacheMembersRolesComponent> {
  TestBed.configureTestingModule({
    imports: [CacheMembersRolesComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: activatedRouteStub() },
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
