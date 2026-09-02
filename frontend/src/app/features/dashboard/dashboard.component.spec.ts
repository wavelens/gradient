/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { DashboardComponent } from './dashboard.component';
import { ProjectsService } from '@core/services/projects.service';
import { CachesService } from '@core/services/caches.service';
import { Cache, Project } from '@core/models';

const PROJECT = {
  id: 'p1',
  name: 'acme',
  display_name: 'Acme',
  description: 'Ships things',
  public: false,
  hide_build_requests: false,
  managed: false,
  role: 'Admin',
} as Project;

const CACHE = {
  id: 'c1',
  name: 'shared',
  display_name: 'Shared',
  description: 'The shared cache',
  active: true,
  priority: 30,
  local_priority: null,
  max_storage_gb: 10,
  public: false,
  managed: false,
  can_edit: true,
} as Cache;

function render(): HTMLElement {
  TestBed.configureTestingModule({
    imports: [DashboardComponent],
    providers: [
      provideRouter([]),
      { provide: ProjectsService, useValue: { getProjects: () => of({ items: [PROJECT] }) } },
      { provide: CachesService, useValue: { getCaches: () => of({ items: [CACHE] }) } },
    ],
  });
  const fixture = TestBed.createComponent(DashboardComponent);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('DashboardComponent', () => {
  it('navigates from every card, index counts included', () => {
    const links = Array.from(render().querySelectorAll('gr-nav-card a')).map((a) =>
      a.getAttribute('href'),
    );
    expect(links).toEqual(['/projects', '/caches', '/project/acme', '/caches/shared']);
  });

  it('puts the entity state on the card meta line rather than in its own box', () => {
    const root = render();
    expect(root.querySelector('.card')).toBeNull();
    const meta = Array.from(root.querySelectorAll('.nav-card__meta')).map((m) =>
      m.textContent?.replace(/\s+/g, ' ').trim(),
    );
    expect(meta).toEqual(['', '', 'Admin', 'Active Priority 30']);
  });
});
