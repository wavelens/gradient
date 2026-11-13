/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { SettingsPageSideNavigationComponent, type VerticalNavigationLink } from './settings-page-side-navigation.component';

@Component({ standalone: true, template: '' })
class PageComponent {}

const projectLinks: VerticalNavigationLink[] = [
  { label: 'General', icon: 'tune', link: ['/project', 'p', 'settings'] },
  { label: 'Workers', icon: 'construction', link: ['/project', 'p', 'workers'] },
  { label: 'Subscriptions', icon: 'hub', link: ['/project', 'p', 'caches'], badge: '2 Pending' },
  { label: 'Project Settings', icon: 'apartment', link: ['/project', 'q', 'settings'], showSeparatorLineBeforeLink: true },
];

async function renderAt(url: string, links: VerticalNavigationLink[]) {
  HTMLElement.prototype.scrollIntoView ??= () => {};
  TestBed.configureTestingModule({ providers: [provideRouter([{ path: '**', component: PageComponent }])] });
  await TestBed.inject(Router).navigateByUrl(url);
  const fixture = TestBed.createComponent(SettingsPageSideNavigationComponent);
  fixture.componentRef.setInput('ariaLabel', 'Project Settings');
  fixture.componentRef.setInput('links', links);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('gr-settings-page-side-navigation', () => {
  it('renders one labelled link per item inside a named navigation landmark', async () => {
    const root = await renderAt('/project/p/settings', projectLinks);
    expect(root.querySelector('nav')?.getAttribute('aria-label')).toBe('Project Settings');
    const links = Array.from(root.querySelectorAll('a'));
    expect(links.map((link) => link.querySelector('.side-navigation-label')?.textContent)).toEqual(['General', 'Workers', 'Subscriptions', 'Project Settings']);
    expect(links.map((link) => link.querySelector('gr-badge')?.textContent?.trim() ?? null)).toEqual([null, null, '2 Pending', null]);
  });

  it('marks the link of the current page, also on pages below it', async () => {
    const root = await renderAt('/project/p/workers/w1/metrics', projectLinks);
    const current = root.querySelectorAll('a[aria-current="page"]');
    expect(current.length).toBe(1);
    expect(current[0].textContent).toContain('Workers');
  });

  it('draws a separator only before links that ask for one', async () => {
    const root = await renderAt('/project/p/settings', projectLinks);
    const separators = root.querySelectorAll('nav hr');
    expect(separators.length).toBe(1);
    expect(separators[0].nextElementSibling?.textContent).toContain('Project Settings');
  });

  it('Badge on a different link -> scroll the link of the current page back to the center', async () => {
    const scrolledElements: HTMLElement[] = [];
    const original = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = function (this: HTMLElement) {
      scrolledElements.push(this);
    };
    onTestFinished(() => {
      HTMLElement.prototype.scrollIntoView = original;
    });
    const withoutBadge = projectLinks.map(({ badge: _badge, ...link }) => link);
    TestBed.configureTestingModule({ providers: [provideRouter([{ path: '**', component: PageComponent }])] });
    await TestBed.inject(Router).navigateByUrl('/project/p/workers');
    const fixture = TestBed.createComponent(SettingsPageSideNavigationComponent);
    fixture.componentRef.setInput('ariaLabel', 'Project Settings');
    fixture.componentRef.setInput('links', withoutBadge);
    fixture.detectChanges();
    await fixture.whenStable();
    scrolledElements.length = 0;

    fixture.componentRef.setInput('links', projectLinks);
    fixture.detectChanges();
    await fixture.whenStable();

    expect(scrolledElements.at(-1)?.textContent).toContain('Workers');
  });

  it('renders nothing when there is only one page to link to', async () => {
    const root = await renderAt('/project/p/settings', projectLinks.slice(0, 1));
    expect(root.querySelector('nav')).toBeNull();
  });
});
