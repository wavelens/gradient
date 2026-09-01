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
    <gr-page-layout title="Integrations" subtitle="Forge webhooks">
      <button slot="actions">New</button>
      <p slot="banner" class="banner">heads up</p>
      <p class="body">content</p>
    </gr-page-layout>
  `,
})
class Host {}

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

  async function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('renders title and subtitle', async () => {
    const root = await render();
    expect(root.querySelector('h1')?.textContent).toContain('Integrations');
    expect(root.querySelector('.page-layout__subtitle')?.textContent).toContain('Forge webhooks');
  });

  it('projects actions, banner and body into their own slots', async () => {
    const root = await render();
    expect(root.querySelector('.page-layout__actions button')).not.toBeNull();
    expect(root.querySelector('.page-layout__banners .banner')).not.toBeNull();
    expect(root.querySelector('.page-layout__body .body')).not.toBeNull();
  });

  it('omits the subtitle when not given', async () => {
    const fixture = TestBed.createComponent(PageLayoutComponent);
    fixture.componentRef.setInput('title', 'Bare');
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.page-layout__subtitle')).toBeNull();
  });
});
