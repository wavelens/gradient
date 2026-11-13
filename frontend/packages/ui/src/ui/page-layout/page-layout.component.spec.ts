/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { PageLayoutComponent } from './page-layout.component';

@Component({
  standalone: true,
  imports: [PageLayoutComponent],
  template: `
    <gr-page-layout title="Integrations" subtitle="Git host webhooks">
      <span slot="after-title" class="state">Managed</span>
      <button slot="actions">New</button>
      <p slot="banner" class="banner">heads up</p>
      <p class="body">content</p>
    </gr-page-layout>
  `,
})
class Host {}

@Component({
  standalone: true,
  imports: [PageLayoutComponent],
  template: `
    <gr-page-layout title="Workers" [breadcrumb]="[{ label: 'Settings' }]">
      <nav slot="settings-navigation" class="pages">General Workers</nav>
      <p class="body">content</p>
    </gr-page-layout>
  `,
})
class HostWithSettingsNavigation {}

describe('gr-page-layout', () => {
  beforeEach(() => TestBed.configureTestingModule({ providers: [provideRouter([])] }));

  async function renderCrumbs(crumbs: unknown) {
    const fixture = TestBed.createComponent(PageLayoutComponent);
    fixture.componentRef.setInput('title', 'Integrations');
    fixture.componentRef.setInput('breadcrumb', crumbs);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('renders no breadcrumb when the trail is empty', async () => {
    expect((await renderCrumbs([])).querySelector('.breadcrumb')).toBeNull();
  });

  it('renders all but the last crumb as links', async () => {
    const root = await renderCrumbs([
      { label: 'my-project', link: ['/project', 'my-project'] },
      { label: 'Settings', link: ['/project', 'my-project', 'settings'] },
      { label: 'Integrations' },
    ]);
    expect(root.querySelectorAll('.breadcrumb-link').length).toBe(2);
    expect(root.querySelectorAll('.breadcrumb-separator').length).toBe(2);
    expect(root.querySelector('.breadcrumb-current')?.textContent).toContain('Integrations');
  });

  it('keeps the full name of every crumb in its title', async () => {
    const root = await renderCrumbs([{ label: 'a-very-long-project-name', link: ['/project', 'a'] }, { label: 'Settings' }]);
    const titles = Array.from(root.querySelectorAll('.breadcrumb > :not(.breadcrumb-separator)')).map((crumb) => crumb.getAttribute('title'));
    expect(titles).toEqual(['a-very-long-project-name', 'Settings']);
  });

  async function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('renders title and subtitle', async () => {
    const root = await render();
    expect(root.querySelector('h1')?.textContent).toContain('Integrations');
    expect(root.querySelector('h1')?.getAttribute('title')).toBe('Integrations');
    expect(root.querySelector('gr-page-layout')?.hasAttribute('title')).toBe(false);
    expect(root.querySelector('.page-layout__subtitle')?.textContent).toContain('Git host webhooks');
  });

  it('projects actions, banner and body into their own slots', async () => {
    const root = await render();
    expect(root.querySelector('.page-layout__actions button')).not.toBeNull();
    expect(root.querySelector('.page-layout__banners .banner')).not.toBeNull();
    expect(root.querySelector('.page-layout__body .body')).not.toBeNull();
  });

  it('takes the detail width unless the page type asks for another', async () => {
    const fixture = TestBed.createComponent(PageLayoutComponent);
    fixture.componentRef.setInput('title', 'Settings');
    fixture.detectChanges();
    const layout = (fixture.nativeElement as HTMLElement).querySelector('.page-layout')!;
    expect(layout.classList).toContain('page-layout--detail');
    fixture.componentRef.setInput('width', 'settings');
    fixture.detectChanges();
    expect(layout.classList).toContain('page-layout--settings');
    expect(layout.classList).not.toContain('page-layout--detail');
  });

  it('puts the after-title content on the title row, directly after the heading', async () => {
    const row = (await render()).querySelector('.page-layout__title-row')!;
    expect(Array.from(row.children).map((child) => child.tagName)).toEqual(['H1', 'SPAN']);
    expect(row.querySelector('h1')?.textContent?.trim()).toBe('Integrations');
  });

  it('puts the breadcrumb above the settings navigation and the heading in the column beside it', async () => {
    const fixture = TestBed.createComponent(HostWithSettingsNavigation);
    fixture.detectChanges();
    await fixture.whenStable();
    const layout = (fixture.nativeElement as HTMLElement).querySelector('.page-layout')!;
    expect(Array.from(layout.children).map((child) => child.className.split(' ')[0] || child.tagName)).toEqual([
      'GR-BREADCRUMB',
      'page-layout__settings-navigation',
      'page-layout__main-column',
    ]);
    expect(layout.querySelector('.page-layout__settings-navigation .pages')).not.toBeNull();
    expect(layout.querySelector('.page-layout__main-column h1')?.textContent).toContain('Workers');
    expect(layout.querySelector('.page-layout__main-column .body')).not.toBeNull();
  });

  it('omits the subtitle when not given', async () => {
    const fixture = TestBed.createComponent(PageLayoutComponent);
    fixture.componentRef.setInput('title', 'Bare');
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.page-layout__subtitle')).toBeNull();
  });
});
