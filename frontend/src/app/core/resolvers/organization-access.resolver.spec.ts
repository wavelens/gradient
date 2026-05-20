/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRouteSnapshot, convertToParamMap } from '@angular/router';
import { Observable, firstValueFrom, of, throwError } from 'rxjs';
import { OrganizationsService } from '@core/services/organizations.service';
import { organizationAccessResolver, OrganizationAccessData } from './organization-access.resolver';
import { Organization } from '@core/models';

function snap(params: Record<string, string>): ActivatedRouteSnapshot {
  return { paramMap: convertToParamMap(params) } as ActivatedRouteSnapshot;
}

function runResolver(route: ActivatedRouteSnapshot): Promise<OrganizationAccessData> {
  const result = TestBed.runInInjectionContext(() =>
    organizationAccessResolver(route, {} as never),
  ) as Observable<OrganizationAccessData>;
  return firstValueFrom(result);
}

describe('organizationAccessResolver', () => {
  let getOrganization: ReturnType<typeof vi.fn>;

  const baseOrg: Organization = {
    id: 'o1',
    name: 'wavelens',
    display_name: 'Wavelens',
    description: '',
    public: true,
    hide_build_requests: false,
    managed: false,
  };

  beforeEach(() => {
    getOrganization = vi.fn(() => of(baseOrg));
    TestBed.configureTestingModule({
      providers: [{ provide: OrganizationsService, useValue: { getOrganization } }],
    });
  });

  it('fetches the organization by route param', async () => {
    const data = await runResolver(snap({ org: 'wavelens' }));
    expect(getOrganization).toHaveBeenCalledWith('wavelens');
    expect(data.organization).toBe(baseOrg);
  });

  it('returns null when the org param is missing', async () => {
    const data = await runResolver(snap({}));
    expect(getOrganization).not.toHaveBeenCalled();
    expect(data.organization).toBeNull();
  });

  it('falls back to null when the fetch errors so navigation still proceeds', async () => {
    getOrganization.mockReturnValue(throwError(() => new Error('boom')));
    const data = await runResolver(snap({ org: 'missing' }));
    expect(data.organization).toBeNull();
  });
});
