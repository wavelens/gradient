/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { ApiKeysComponent } from './api-keys.component';
import { UserService } from '@core/services/user.service';
import { OrganizationsService } from '@core/services/organizations.service';
import { ApiKey } from '@core/models/user.model';

const unmanagedKey: ApiKey = {
  id: 'k1',
  name: 'CI key',
  managed: false,
  permissions: ['viewOrg'],
  organization: null,
  created_at: '2026-01-01T00:00:00',
  last_used_at: null,
  expires_at: null,
  revoked_at: null,
};

const managedKey: ApiKey = {
  id: 'k2',
  name: 'Nix key',
  managed: true,
  permissions: ['viewOrg'],
  organization: null,
  created_at: '2026-01-01T00:00:00',
  last_used_at: null,
  expires_at: null,
  revoked_at: null,
};

function setup(keys: ApiKey[]): ComponentFixture<ApiKeysComponent> {
  TestBed.configureTestingModule({
    imports: [ApiKeysComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: UserService,
        useValue: {
          getApiKeys: () => of(keys),
          getApiKeyPermissions: () => of({ available_permissions: [] }),
        },
      },
      {
        provide: OrganizationsService,
        useValue: { getOrganizations: () => of({ items: [], total: 0, page: 1, per_page: 100 }) },
      },
    ],
  });
  const fixture = TestBed.createComponent(ApiKeysComponent);
  fixture.detectChanges();
  return fixture;
}

function buttonsByText(root: HTMLElement, text: string): HTMLButtonElement[] {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).filter(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  );
}

describe('ApiKeysComponent - managed-key gating', () => {
  it('renders Edit / Revoke / Delete enabled for an unmanaged key', () => {
    const fixture = setup([unmanagedKey]);
    const editButtons = buttonsByText(fixture.nativeElement, 'edit');
    const revokeButtons = buttonsByText(fixture.nativeElement, 'revoke');
    const deleteButtons = buttonsByText(fixture.nativeElement, 'delete');
    expect(editButtons.length).toBe(1);
    expect(editButtons[0].disabled).toBe(false);
    expect(revokeButtons.length).toBe(1);
    expect(revokeButtons[0].disabled).toBe(false);
    expect(deleteButtons.length).toBe(1);
    expect(deleteButtons[0].disabled).toBe(false);
  });

  it('renders Revoke / Delete present-but-disabled for a managed key', () => {
    const fixture = setup([managedKey]);
    const revokeButtons = buttonsByText(fixture.nativeElement, 'revoke');
    const deleteButtons = buttonsByText(fixture.nativeElement, 'delete');
    expect(revokeButtons.length).toBe(1);
    expect(revokeButtons[0].disabled).toBe(true);
    expect(deleteButtons.length).toBe(1);
    expect(deleteButtons[0].disabled).toBe(true);
  });

  it('renders Edit present-but-disabled for a managed key', () => {
    const fixture = setup([managedKey]);
    const editButtons = buttonsByText(fixture.nativeElement, 'edit');
    expect(editButtons.length).toBe(1);
    expect(editButtons[0].disabled).toBe(true);
  });
});
