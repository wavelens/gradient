/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import type { TaskResponse } from '@core/models';
import { TaskCardComponent } from './task-card.component';

const run = (id: string) => ({ id, status: 'Completed', duration_ms: 60_000, created_at: '2026-11-10 12:00:00', started_at: '2026-11-10 12:05:00', finished_at: '2026-11-10 12:10:00' });

function render(overrides: Partial<TaskResponse> = {}, latestEvaluationStatusText: string | null = null) {
  const task = { id: 't1', name: 'build', display_name: 'Nightly Build', description: 'Nightly build', active: true, recent_evaluations: [], ...overrides } as unknown as TaskResponse;
  TestBed.configureTestingModule({ imports: [TaskCardComponent], providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(TaskCardComponent);
  fixture.componentRef.setInput('task', task);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('latestEvaluationStatusText', latestEvaluationStatusText);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('TaskCardComponent', () => {
  it('shows the display name and no slug', () => {
    const root = render();
    expect(root.querySelector('.task-card__name')?.textContent?.trim()).toBe('Nightly Build');
    expect(root.querySelector('.entity-name__slug')).toBeNull();
  });

  it('titles the link with the display name and the slug', () => {
    expect(render().querySelector('a')?.getAttribute('title')).toBe('Nightly Build (build)');
  });

  it('keeps the description element when the task has no description', () => {
    const description = render({ description: '' }).querySelector('.task-card__description');
    expect(description).not.toBeNull();
    expect(description?.textContent?.trim()).toBe('');
  });

  it('exposes the full description as a title', () => {
    expect(render().querySelector('.task-card__description')?.getAttribute('title')).toBe('Nightly build');
  });

  it('marks an inactive task with a neutral badge and an active task with none', () => {
    expect(render({ active: false }).querySelector('gr-badge .badge')?.classList).toContain('is-neutral');
    TestBed.resetTestingModule();
    expect(render({ active: true }).querySelector('gr-badge .badge')).toBeNull();
  });

  it('shows the latest evaluation text and draws the recent evaluations without links', () => {
    const root = render({ recent_evaluations: [run('e1'), run('e2')] } as Partial<TaskResponse>, 'Finished 12m ago');
    expect(root.querySelector('.task-card__latest-evaluation')?.textContent?.trim()).toBe('Finished 12m ago');
    expect(root.querySelectorAll('app-evaluation-history .bar')).toHaveLength(2);
    expect(root.querySelector('app-evaluation-history a')).toBeNull();
  });

  it('draws no history for a single recent evaluation', () => {
    expect(render({ recent_evaluations: [run('e1')] } as Partial<TaskResponse>).querySelector('app-evaluation-history')).toBeNull();
  });

  it('links to the task page', () => {
    expect(render().querySelector('a')?.getAttribute('href')).toMatch(/\/project\/acme\/task\/build$/);
  });

  it('renders the arrow', () => {
    expect(render().querySelector('.task-card__arrow')).not.toBeNull();
  });
});
