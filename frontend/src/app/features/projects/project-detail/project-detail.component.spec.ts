/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { ProjectDetailComponent } from './project-detail.component';
import { AuthService } from '@core/services/auth.service';
import { ProjectsService } from '@core/services/projects.service';
import { StarsService } from '@core/services/stars.service';
import { TasksService } from '@core/services/tasks.service';

describe('ProjectDetailComponent header', () => {
  function render(authenticated: boolean) {
    const starred = vi.fn(() => of(true));
    TestBed.configureTestingModule({
      imports: [ProjectDetailComponent],
      providers: [
        provideRouter([]),
        { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme' }) } } },
        { provide: AuthService, useValue: { isAuthenticated: () => authenticated } },
        { provide: ProjectsService, useValue: { getProject: () => of({ name: 'acme', display_name: 'Acme', description: '', role: null }) } },
        { provide: TasksService, useValue: { getTasks: () => of({ items: [], total: 0, page: 1 }) } },
        { provide: StarsService, useValue: { starred, set: () => of(true) } },
      ],
    });
    const fixture = TestBed.createComponent(ProjectDetailComponent);
    fixture.detectChanges();
    return { root: fixture.nativeElement as HTMLElement, starred };
  }

  it('stars the project from its header', () => {
    const { root, starred } = render(true);
    expect(starred).toHaveBeenCalledWith({ kind: 'project', project: 'acme' });
    expect(root.querySelector('gr-star-button button')!.getAttribute('aria-pressed')).toBe('true');
    expect(root.querySelector('gr-star-button button')!.textContent!.trim()).toBe('Starred');
  });

  it('shows no star to a guest', () => {
    expect(render(false).root.querySelector('gr-star-button')).toBeNull();
  });
});
