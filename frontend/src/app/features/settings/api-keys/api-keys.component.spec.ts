/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { ApiKeysComponent } from './api-keys.component';
import { UserService } from '@core/services/user.service';
import { ApiError } from '@core/services/api.service';
import { ProjectsService } from '@core/services/projects.service';
import { ApiKeyInfo } from '@core/models';

const unmanagedKey: ApiKeyInfo = {
  id: 'k1',
  name: 'CI key',
  managed: false,
  permissions: ['viewProject'],
  project: null,
  created_at: '2026-01-01T00:00:00',
  last_used_at: null,
  expires_at: null,
  revoked_at: null,
  allowed_ips: [],
};

const managedKey: ApiKeyInfo = {
  id: 'k2',
  name: 'Nix key',
  managed: true,
  permissions: ['viewProject'],
  project: null,
  created_at: '2026-01-01T00:00:00',
  last_used_at: null,
  expires_at: null,
  revoked_at: null,
  allowed_ips: [],
};

function setup(keys: ApiKeyInfo[], user: object = {}): ComponentFixture<ApiKeysComponent> {
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
          ...user,
        },
      },
      {
        provide: ProjectsService,
        useValue: { getProjects: () => of({ items: [], total: 0, page: 1, per_page: 100 }) },
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

describe('ApiKeysComponent - allowed sources', () => {
  it('refuses to send entries that are not IP addresses or CIDRs', () => {
    const createApiKey = vi.fn(() => of('key'));
    const fixture = setup([], { createApiKey });
    const component = fixture.componentInstance;
    component.formName = 'deploy';
    component.formPermissions = { viewProject: true };
    component.formAllowedIps = '10.0.0.0/8\nci.example.com\n';
    expect(component.invalidIps()).toEqual(['ci.example.com']);
    component.saveKey();
    expect(createApiKey).not.toHaveBeenCalled();

    component.formAllowedIps = '10.0.0.0/8\n 2001:db8::/32 ';
    component.saveKey();
    expect(createApiKey).toHaveBeenCalledWith('deploy', null, ['viewProject'], null, null, ['10.0.0.0/8', '2001:db8::/32']);
  });
});

describe('ApiKeysComponent - create errors', () => {
  it('shows the reason the server gave for refusing the key', () => {
    const reason = 'Permission manageProject exceeds your own permissions';
    const fixture = setup([], { createApiKey: () => throwError(() => new ApiError(reason, 403)) });
    const component = fixture.componentInstance;
    component.formName = 'deploy';
    component.formPermissions = { viewProject: true };
    component.saveKey();
    expect(component.errorMessage()).toBe(reason);
  });
});
