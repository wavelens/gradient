/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, inject } from '@angular/core';
import { firstValueFrom } from 'rxjs';
import { ProjectsService } from './projects.service';
import { AccessState } from '@core/models/access.model';

@Injectable({ providedIn: 'root' })
export class ProjectAccessService {
  private projects = inject(ProjectsService);

  async forProject(name: string): Promise<AccessState> {
    const project = await firstValueFrom(this.projects.getProject(name));
    const canEdit = !!project.role && project.role !== 'View';
    // Projects have no distinct trigger permission - mirror canEdit so callers
    // that branch on canTrigger behave identically for project-scoped pages.
    return { managed: !!project.managed, canEdit, canTrigger: canEdit };
  }
}
