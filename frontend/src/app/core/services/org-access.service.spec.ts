/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { OrgAccessService } from './org-access.service';
import { OrganizationsService } from './organizations.service';
import { Organization } from '@core/models/organization.model';

function org(partial: Partial<Organization> = {}): Organization {
  return {
    id: 'o1',
    name: 'acme',
    display_name: 'Acme',
    description: '',
    public: false,
    managed: false,
    ...partial,
  };
}

describe('OrgAccessService', () => {
  let svc: OrgAccessService;
  let getOrganization: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    getOrganization = vi.fn();
    TestBed.configureTestingModule({
      providers: [{ provide: OrganizationsService, useValue: { getOrganization } }],
    });
    svc = TestBed.inject(OrgAccessService);
  });

  it('returns canEdit=true and managed=false for Admin in unmanaged org', async () => {
    getOrganization.mockReturnValue(of(org({ role: 'Admin' })));
    expect(await svc.forOrg('acme')).toEqual({ managed: false, canEdit: true });
  });

  it('returns canEdit=true and managed=true for Admin in managed org', async () => {
    getOrganization.mockReturnValue(of(org({ role: 'Admin', managed: true })));
    expect(await svc.forOrg('acme')).toEqual({ managed: true, canEdit: true });
  });

  it('returns canEdit=true for Write role', async () => {
    getOrganization.mockReturnValue(of(org({ role: 'Write' })));
    expect(await svc.forOrg('acme')).toEqual({ managed: false, canEdit: true });
  });

  it('returns canEdit=false for View role', async () => {
    getOrganization.mockReturnValue(of(org({ role: 'View' })));
    expect(await svc.forOrg('acme')).toEqual({ managed: false, canEdit: false });
  });

  it('returns canEdit=false when role is missing (not a member)', async () => {
    getOrganization.mockReturnValue(of(org({ role: undefined })));
    expect(await svc.forOrg('acme')).toEqual({ managed: false, canEdit: false });
  });

  it('treats custom non-View role names as writable', async () => {
    getOrganization.mockReturnValue(of(org({ role: 'Maintainer' as never })));
    expect(await svc.forOrg('acme')).toEqual({ managed: false, canEdit: true });
  });
});
