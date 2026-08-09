/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { ProjectAccessService } from './project-access.service';
import { ProjectsService } from './projects.service';
import { Project } from '@core/models/project.model';

function project(partial: Partial<Project> = {}): Project {
  return {
    id: 'o1',
    name: 'acme',
    display_name: 'Acme',
    description: '',
    public: false,
    hide_build_requests: false,
    managed: false,
    ...partial,
  };
}

describe('ProjectAccessService', () => {
  let svc: ProjectAccessService;
  let getProject: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    getProject = vi.fn();
    TestBed.configureTestingModule({
      providers: [{ provide: ProjectsService, useValue: { getProject } }],
    });
    svc = TestBed.inject(ProjectAccessService);
  });

  it('returns canEdit=true and managed=false for Admin in unmanaged project', async () => {
    getProject.mockReturnValue(of(project({ role: 'Admin' })));
    expect(await svc.forProject('acme')).toEqual({ managed: false, canEdit: true, canTrigger: true });
  });

  it('returns canEdit=true and managed=true for Admin in managed project', async () => {
    getProject.mockReturnValue(of(project({ role: 'Admin', managed: true })));
    expect(await svc.forProject('acme')).toEqual({ managed: true, canEdit: true, canTrigger: true });
  });

  it('returns canEdit=true for Write role', async () => {
    getProject.mockReturnValue(of(project({ role: 'Write' })));
    expect(await svc.forProject('acme')).toEqual({ managed: false, canEdit: true, canTrigger: true });
  });

  it('returns canEdit=false for View role', async () => {
    getProject.mockReturnValue(of(project({ role: 'View' })));
    expect(await svc.forProject('acme')).toEqual({ managed: false, canEdit: false, canTrigger: false });
  });

  it('returns canEdit=false when role is missing (not a member)', async () => {
    getProject.mockReturnValue(of(project({ role: undefined })));
    expect(await svc.forProject('acme')).toEqual({ managed: false, canEdit: false, canTrigger: false });
  });

  it('treats custom non-View role names as writable', async () => {
    getProject.mockReturnValue(of(project({ role: 'Maintainer' as never })));
    expect(await svc.forProject('acme')).toEqual({ managed: false, canEdit: true, canTrigger: true });
  });
});
