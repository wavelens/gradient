/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { inject } from '@angular/core';
import { ResolveFn } from '@angular/router';
import { catchError, map, of } from 'rxjs';
import { OrganizationsService } from '@core/services/organizations.service';
import { Organization } from '@core/models';

export interface OrganizationAccessData {
  organization: Organization | null;
}

export const organizationAccessResolver: ResolveFn<OrganizationAccessData> = (route) => {
  const orgs = inject(OrganizationsService);
  const name = route.paramMap.get('org') ?? '';
  if (!name) return of({ organization: null });
  return orgs.getOrganization(name).pipe(
    map((organization) => ({ organization })),
    catchError(() => of({ organization: null })),
  );
};
