/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { firstValueFrom } from 'rxjs';
import { OrganizationsService } from './organizations.service';
import { AccessState } from '@core/models/access.model';

@Injectable({ providedIn: 'root' })
export class OrgAccessService {
  private orgs = inject(OrganizationsService);

  async forOrg(name: string): Promise<AccessState> {
    const org = await firstValueFrom(this.orgs.getOrganization(name));
    const canEdit = !!org.role && org.role !== 'View';
    return { managed: !!org.managed, canEdit };
  }
}
