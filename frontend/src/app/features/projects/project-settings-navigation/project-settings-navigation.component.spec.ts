/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, Router, provideRouter } from '@angular/router';
import type { Permission } from '@core/models';
import { projectRouteStub } from '@core/resolvers/access-route.testing';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ProjectSettingsNavigationComponent } from './project-settings-navigation.component';

@Component({ template: '' })
class EmptyRouteTargetComponent {}

interface NavigationOptions {
  permissions: Permission[];
  role: string | null;
  execEnabled?: boolean;
  superuser?: boolean;
  url?: string;
}

async function configureTestBedNavigateAndRenderProjectSettingsSideNavigation(options: NavigationOptions) {
  HTMLElement.prototype.scrollIntoView ??= () => {};
  TestBed.configureTestingModule({
    providers: [
      provideRouter([{ path: '**', component: EmptyRouteTargetComponent }]),
      { provide: AuthService, useValue: { user: signal({ superuser: options.superuser ?? false }) } },
      { provide: ConfigService, useValue: { execEnabled: options.execEnabled ?? true } },
      {
        provide: ActivatedRoute,
        useValue: projectRouteStub('acme', { managed: false, canEdit: false, canTrigger: false }, { permissions: options.permissions, role: options.role }),
      },
    ],
  });
  if (options.url) await TestBed.inject(Router).navigateByUrl(options.url);
  const fixture = TestBed.createComponent(ProjectSettingsNavigationComponent);
  fixture.componentRef.setInput('projectUrlName', 'acme');
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

async function linkLabels(options: NavigationOptions) {
  const root = await configureTestBedNavigateAndRenderProjectSettingsSideNavigation(options);
  return Array.from(root.querySelectorAll('a')).map((link) => [link.querySelector('.side-navigation-label')?.textContent, link.getAttribute('href')]);
}

describe('ProjectSettingsNavigationComponent', () => {
  it('links every project settings page for a member who can change settings and event notifications', async () => {
    expect(await linkLabels({ permissions: ['viewProject', 'manageProjectSettings', 'manageWebhooks'], role: 'Admin' })).toEqual([
      ['General', '/project/acme/settings'],
      ['Members', '/project/acme/members'],
      ['Workers', '/project/acme/workers'],
      ['Cache subscriptions', '/project/acme/caches'],
      ['Git host connections', '/project/acme/git-host-connections'],
      ['Secrets', '/project/acme/secrets'],
      ['Event notifications', '/project/acme/webhooks'],
    ]);
  });

  it('leaves out secrets and event notifications for a member who can only view', async () => {
    expect((await linkLabels({ permissions: ['viewProject'], role: 'View' })).map(([label]) => label))
      .toEqual(['General', 'Members', 'Workers', 'Cache subscriptions', 'Git host connections']);
  });

  it('leaves out secrets when the server has exec jobs turned off', async () => {
    const labels = (await linkLabels({ permissions: ['viewProject', 'manageProjectSettings'], role: 'Admin', execEnabled: false })).map(([label]) => label);
    expect(labels).not.toContain('Secrets');
  });

  it('shows no navigation to a visitor who is not a project member', async () => {
    expect(await linkLabels({ permissions: [], role: null })).toEqual([]);
  });

  it('link Members for an instance administrator outside the project', async () => {
    expect((await linkLabels({ permissions: [], role: null, superuser: true })).map(([label]) => label)).toEqual(['General', 'Members']);
  });

  it.each(['people', 'teams', 'roles'])('mark Members as the current page on every Members tab (%s)', async (tab) => {
    const root = await configureTestBedNavigateAndRenderProjectSettingsSideNavigation({ permissions: ['viewProject'], role: 'View', url: `/project/acme/members/${tab}` });
    const active = Array.from(root.querySelectorAll('a.is-active'), (link) => link.querySelector('.side-navigation-label')?.textContent);
    expect(active).toEqual(['Members']);
  });
});
