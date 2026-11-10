/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { LinkTabsComponent, type LinkTab } from './link-tabs.component';

@Component({ standalone: true, template: '' })
class PageComponent {}

async function renderTabsOnInvitesPage(tabs: LinkTab[]) {
  const scrolled: { element: HTMLElement; options: unknown }[] = [];
  const original = HTMLElement.prototype.scrollIntoView;
  HTMLElement.prototype.scrollIntoView = function (this: HTMLElement, options) {
    scrolled.push({ element: this, options });
  };
  onTestFinished(() => {
    HTMLElement.prototype.scrollIntoView = original;
  });
  TestBed.configureTestingModule({
    providers: [provideRouter([{ path: '**', component: PageComponent }])],
  });
  await TestBed.inject(Router).navigateByUrl('/settings/invites');
  const fixture = TestBed.createComponent(LinkTabsComponent);
  fixture.componentRef.setInput('ariaLabel', 'Settings');
  fixture.componentRef.setInput('tabs', tabs);
  const settle = async () => {
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  };
  await settle();
  return { fixture, scrolled, settle };
}

describe('gr-link-tabs', () => {
  it('scrolls the active tab to the middle of the row, clear of the edge fade', async () => {
    const { fixture, scrolled } = await renderTabsOnInvitesPage([
      { label: 'Profile', link: ['/settings', 'profile'] },
      { label: 'Invites', link: ['/settings', 'invites'] },
    ]);

    const active = (fixture.nativeElement as HTMLElement).querySelector('.is-active');
    expect(active?.textContent).toBe('Invites');
    expect(scrolled.map((call) => call.options)).toContainEqual({ block: 'nearest', inline: 'center' });
  });

  it('scrolls the active tab back into view after its label changes', async () => {
    const { fixture, scrolled, settle } = await renderTabsOnInvitesPage([
      { label: 'Profile', link: ['/settings', 'profile'] },
      { label: 'Invites', link: ['/settings', 'invites'] },
    ]);
    scrolled.length = 0;

    fixture.componentRef.setInput('tabs', [
      { label: 'Profile', link: ['/settings', 'profile'] },
      { label: 'Invites (6)', link: ['/settings', 'invites'] },
    ]);
    await settle();

    const active = (fixture.nativeElement as HTMLElement).querySelector<HTMLElement>('.is-active');
    expect(active?.textContent).toBe('Invites (6)');
    expect(scrolled.at(-1)?.element).toBe(active);
  });

  it('scrolls the active tab link back to the middle when it changes size', async () => {
    const observed: { target: Element; notify: () => void }[] = [];
    vi.stubGlobal('ResizeObserver', class {
      constructor(private readonly callback: () => void) {}
      observe(target: Element) {
        observed.push({ target, notify: this.callback });
      }
      disconnect() {}
    });
    onTestFinished(() => {
      vi.unstubAllGlobals();
    });
    const { fixture, scrolled } = await renderTabsOnInvitesPage([
      { label: 'Profile', link: ['/settings', 'profile'] },
      { label: 'Invites', link: ['/settings', 'invites'] },
    ]);
    const active = (fixture.nativeElement as HTMLElement).querySelector<HTMLElement>('.is-active');
    const watch = observed.filter((entry) => entry.target === active).at(-1);
    scrolled.length = 0;

    watch?.notify();

    expect(scrolled).toEqual([{ element: active, options: { block: 'nearest', inline: 'center' } }]);
  });
});
