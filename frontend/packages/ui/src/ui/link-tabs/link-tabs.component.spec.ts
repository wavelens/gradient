/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { LinkTabsComponent } from './link-tabs.component';

@Component({ standalone: true, template: '' })
class PageComponent {}

describe('gr-link-tabs', () => {
  it('scrolls the active tab to the middle of the row, clear of the edge fade', async () => {
    const scrolled: unknown[] = [];
    const original = HTMLElement.prototype.scrollIntoView;
    HTMLElement.prototype.scrollIntoView = (options) => {
      scrolled.push(options);
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
    fixture.componentRef.setInput('tabs', [
      { label: 'Profile', link: ['/settings', 'profile'] },
      { label: 'Invites', link: ['/settings', 'invites'] },
    ]);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();

    const active = (fixture.nativeElement as HTMLElement).querySelector('.is-active');
    expect(active?.textContent).toBe('Invites');
    expect(scrolled).toContainEqual({ block: 'nearest', inline: 'center' });
  });
});
