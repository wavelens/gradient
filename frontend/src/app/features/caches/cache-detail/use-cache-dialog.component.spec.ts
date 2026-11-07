/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectsService } from '@core/services/projects.service';
import { UseCacheDialogComponent } from './use-cache-dialog.component';

function render(items: { name: string; display_name: string; managed: boolean; permissions: string[] }[], canCreate = true) {
  TestBed.configureTestingModule({
    imports: [UseCacheDialogComponent],
    providers: [
      provideRouter([]),
      { provide: ProjectsService, useValue: { getProjects: () => of({ items, total: items.length, page: 1 }) } },
      { provide: ConfigService, useValue: { createProject: 'everyone', canCreate: () => canCreate } },
      { provide: AuthService, useValue: { user: () => null } },
    ],
  });
  const fixture = TestBed.createComponent(UseCacheDialogComponent);
  fixture.componentRef.setInput('cache', 'nixpkgs');
  fixture.componentInstance.visible.set(true);
  fixture.detectChanges();
  return fixture;
}

describe('UseCacheDialogComponent', () => {
  it('offers only unmanaged projects whose subscriptions the user may manage and opens their subscriptions with the cache selected', () => {
    const fixture = render([
      { name: 'web', display_name: 'Web', managed: false, permissions: ['viewProject', 'manageSubscriptions'] },
      { name: 'docs', display_name: 'Docs', managed: false, permissions: ['viewProject', 'manageProjectSettings'] },
      { name: 'infra', display_name: 'Infra', managed: true, permissions: ['viewProject', 'manageSubscriptions'] },
    ]);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    const continueButton = Array.from(document.querySelectorAll<HTMLButtonElement>('.gr-dialog button'))
      .find((button) => button.textContent?.includes('Continue'));
    continueButton?.click();
    expect(String(navigate.mock.calls[0][0])).toBe('/project/web/caches?subscribe=nixpkgs');
    expect(fixture.componentInstance.visible()).toBe(false);
  });

  it('links to the project list to create one when the user has none', () => {
    render([]);
    expect(document.querySelector('.gr-dialog a[href="/projects"]')?.textContent).toContain('Create project');
  });

  it('offers no create link to someone who may not create projects', () => {
    render([], false);
    expect(document.querySelector('.gr-dialog a[href="/projects"]')).toBeNull();
  });
});
