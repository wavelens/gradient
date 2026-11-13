/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { taskAccessData } from '@core/resolvers/access-route.testing';
import { TaskDetailsResponse } from '@core/models';
import { TaskSettingsNavigationComponent } from './task-settings-navigation.component';

async function render(canEdit: boolean, task: Partial<TaskDetailsResponse> = {}) {
  HTMLElement.prototype.scrollIntoView ??= () => {};
  TestBed.configureTestingModule({
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { parent: { data: of(taskAccessData({ managed: false, canEdit, canTrigger: canEdit }, task)) } } },
    ],
  });
  const fixture = TestBed.createComponent(TaskSettingsNavigationComponent);
  fixture.componentRef.setInput('projectUrlName', 'acme');
  fixture.componentRef.setInput('taskUrlName', 'build');
  fixture.detectChanges();
  await fixture.whenStable();
  return fixture.nativeElement as HTMLElement;
}

const labels = (root: HTMLElement) => Array.from(root.querySelectorAll('.side-navigation-label')).map((label) => label.textContent);

describe('TaskSettingsNavigationComponent', () => {
  it('ends with a separated link to the project settings for a viewer who can change them', async () => {
    const root = await render(true);
    expect(labels(root)).toEqual(['General', 'Repository', 'Triggers', 'Actions', 'Flake inputs', 'Project Settings']);
    expect(root.querySelector('hr + a')?.getAttribute('href')).toBe('/project/acme/settings');
  });

  it('leaves out the project settings link for a viewer who cannot change them', async () => {
    const root = await render(false);
    expect(labels(root)).toEqual(['General', 'Repository', 'Triggers', 'Actions', 'Flake inputs']);
    expect(root.querySelector('hr')).toBeNull();
  });

  it('Does not render anything for a visitor outside the project (access to General only)', async () => {
    const root = await render(false, { permissions: [] });
    expect(root.querySelector('nav')).toBeNull();
  });
});
