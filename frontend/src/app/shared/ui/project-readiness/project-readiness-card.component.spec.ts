/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import type { Permission, ProjectReadinessResponse } from '@core/models';
import { NotifyService } from '@core/services/notify.service';
import { ProjectsService } from '@core/services/projects.service';
import { TeamsService } from '@core/services/teams.service';
import { ProjectReadinessCardComponent } from './project-readiness-card.component';

const ADMIN: Permission[] = ['viewProject', 'manageWorkers', 'manageSubscriptions', 'editTask'];
const VIEWER: Permission[] = ['viewProject'];

const infra = { name: 'infra', display_name: 'Infra', workers: 2 };
const design = { name: 'design', display_name: 'Design', workers: 0 };
const builders = { name: 'builders', display_name: 'Builders', workers: 3 };

function readiness(overrides: Partial<ProjectReadinessResponse> = {}): ProjectReadinessResponse {
  return {
    ready: false,
    workers: { ready: false, registered: 0, teams: [], teams_granted_without_workers: [design], teams_you_can_grant: [builders] },
    cache: {
      ready: false,
      subscriptions: [{ id: 'c1', name: 'shared', display_name: 'Shared', mode: 'ReadWrite', status: 'pending', managed: false, sign_outputs: true, approvers: ['dave'] }],
    },
    repository: {
      status: 'failing',
      tasks: [
        { name: 'web', display_name: 'Web App', status: 'failing', error: 'Permission denied (publickey)' },
        { name: 'docs', display_name: 'Docs', status: 'verified' },
      ],
    },
    ...overrides,
  };
}

function render(response: ProjectReadinessResponse, permissions: Permission[] = ADMIN) {
  const getProjectReadiness = vi.fn(() => of(response));
  const updateProjectGrant = vi.fn(() => of('Team grant updated'));
  const grantProject = vi.fn(() => of('Request sent'));
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [ProjectReadinessCardComponent],
    providers: [
      provideRouter([]),
      { provide: ProjectsService, useValue: { getProjectReadiness } },
      { provide: TeamsService, useValue: { updateProjectGrant, grantProject } },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(ProjectReadinessCardComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('projectPermissions', permissions);
  fixture.detectChanges();
  const root = fixture.nativeElement as HTMLElement;
  const row = (name: string) => root.querySelector<HTMLElement>(`[data-testid="readiness-${name}"]`)!;
  const buttonLabels = () => Array.from(root.querySelectorAll('button[grButton], a[grButton]'))
    .map((button) => button.querySelector('.gr-button__label')?.textContent?.trim());
  return { fixture, root, row, buttonLabels, getProjectReadiness, updateProjectGrant, grantProject, notify };
}

describe('ProjectReadinessCardComponent', () => {
  it('names the source of every worker and the writable cache of a ready project', () => {
    const { row } = render(readiness({
      ready: true,
      workers: { ready: true, registered: 1, teams: [infra], teams_granted_without_workers: [], teams_you_can_grant: [] },
      cache: { ready: true, subscriptions: [{ id: 'c2', name: 'main', display_name: 'Main', mode: 'ReadWrite', status: 'active', managed: false, sign_outputs: true, approvers: [] }] },
      repository: { status: 'verified', tasks: [{ name: 'web', display_name: 'Web App', status: 'verified' }] },
    }));
    expect(row('workers').textContent).toContain('2 from');
    expect(row('workers').querySelector('gr-entity-name')?.textContent).toContain('Infra');
    expect(row('workers').textContent).toContain('(grant with workers)');
    expect(row('workers').textContent).toContain('1 registered');
    expect(row('workers').querySelector('gr-badge .badge')?.classList).toContain('is-success');
    expect(row('cache').querySelector('gr-entity-name')?.textContent).toContain('Main');
    expect(row('git').querySelector('gr-badge')?.textContent?.trim()).toBe('Verified');
  });

  it('says what is missing in every row, with the failing task and its error', () => {
    const { row } = render(readiness());
    expect(row('workers').textContent).toContain('No workers');
    expect(row('workers').textContent).toContain('Design');
    expect(row('workers').textContent).toContain('has a grant without workers');
    expect(row('workers').querySelector('gr-badge .badge')?.classList).toContain('is-warning');
    expect(row('cache').textContent).toContain('None writable');
    expect(row('cache').textContent).toContain('Approval pending for');
    expect(row('git').querySelector('gr-badge .badge')?.classList).toContain('is-danger');
    expect(row('git').textContent).toContain('Web App');
    expect(row('git').textContent).toContain('Permission denied (publickey)');
    expect(row('git').textContent).not.toContain('Docs');
  });

  it('marks git access as unchecked while no task has fetched yet', () => {
    const { row } = render(readiness({ repository: { status: 'unchecked', tasks: [] } }));
    expect(row('git').querySelector('gr-badge')?.textContent?.trim()).toBe('Unchecked');
    expect(row('git').textContent).toContain('No repository connected yet');
  });

  it('offers each fix to a member with the permission for it', () => {
    const { root, buttonLabels } = render(readiness());
    expect(buttonLabels()).toEqual(['Enable workers', 'Grant to Builders including workers', 'Register worker', 'Subscribe to cache', 'Open repository']);
    const hrefs = Array.from(root.querySelectorAll('a[grButton]')).map((link) => link.getAttribute('href'));
    expect(hrefs).toEqual(['/project/acme/workers', '/project/acme/caches', '/project/acme/task/web/repository']);
    expect(root.querySelector('[aria-label="Enable workers for Design (design)"]')).not.toBeNull();
  });

  it('shows a viewer the rows without any fix', () => {
    const { root, row, buttonLabels } = render(readiness(), VIEWER);
    expect(row('cache').textContent).toContain('None writable');
    expect(buttonLabels()).toEqual([]);
    expect(root.querySelector('a[grButton], button[grButton]')).toBeNull();
  });

  it('enables the workers of a granted team and loads the readiness again', () => {
    const { root, updateProjectGrant, getProjectReadiness, notify } = render(readiness());
    root.querySelector<HTMLButtonElement>('[aria-label="Enable workers for Design (design)"]')!.click();
    expect(updateProjectGrant).toHaveBeenCalledWith('acme', 'design', { workers: true });
    expect(notify.success).toHaveBeenCalledWith('Workers enabled');
    expect(getProjectReadiness).toHaveBeenCalledTimes(2);
  });

  it('grants a team of the viewer with its workers only', () => {
    const { root, grantProject, notify } = render(readiness());
    root.querySelector<HTMLButtonElement>('[aria-label="Grant to Builders (builders) including workers"]')!.click();
    expect(grantProject).toHaveBeenCalledWith('acme', { team: 'builders', users: false, workers: true });
    expect(notify.success).toHaveBeenCalledWith('Request sent');
  });

  it('sends no request for a visitor outside the project', () => {
    const { root, getProjectReadiness } = render(readiness(), []);
    expect(getProjectReadiness).not.toHaveBeenCalled();
    expect(root.querySelector('[data-testid="readiness-workers"]')).toBeNull();
  });
});
