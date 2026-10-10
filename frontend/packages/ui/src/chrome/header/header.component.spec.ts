/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter, Router } from '@angular/router';
import { HeaderComponent } from './header.component';

@Component({ standalone: true, template: '' })
class Page {}

@Component({
  standalone: true,
  imports: [HeaderComponent],
  template: `<gr-header [brand]="{ label: 'Gradient', href: '/' }" [nav]="nav" />`,
})
class Host {
  nav = [
    { label: 'Dashboard', href: '/', exact: true },
    { label: 'Projects', href: '/projects' },
  ];
}

describe('gr-header menu', () => {
  let fixture: ComponentFixture<Host>;

  beforeEach(() => {
    TestBed.configureTestingModule({
      providers: [provideRouter([{ path: '**', component: Page }])],
    });
    fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
  });

  const root = () => fixture.nativeElement as HTMLElement;
  const toggle = () => root().querySelector('.menu-toggle') as HTMLButtonElement;
  const isOpen = () => toggle().getAttribute('aria-expanded') === 'true';

  function openMenu(): void {
    toggle().click();
    fixture.detectChanges();
  }

  it('opens and closes from the menu button', () => {
    openMenu();
    expect(isOpen()).toBe(true);
    expect(root().querySelector('.main-header')?.classList).toContain('menu-open');

    openMenu();
    expect(isOpen()).toBe(false);
  });

  it('closes after a navigation', async () => {
    openMenu();
    await TestBed.inject(Router).navigateByUrl('/projects');
    fixture.detectChanges();
    expect(isOpen()).toBe(false);
  });

  it('closes on a backdrop tap and on Escape', () => {
    openMenu();
    (root().querySelector('.header-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(isOpen()).toBe(false);

    openMenu();
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    fixture.detectChanges();
    expect(isOpen()).toBe(false);
  });

  it('keeps a link active across query parameters and marks an exact link on its own path only', async () => {
    const active = () => Array.from(root().querySelectorAll('.header-nav a.active'), (link) => link.textContent?.trim());
    const router = TestBed.inject(Router);

    await router.navigateByUrl('/?filter=failing');
    fixture.detectChanges();
    expect(active()).toEqual(['Dashboard']);

    await router.navigateByUrl('/projects/demo?page=2');
    fixture.detectChanges();
    expect(active()).toEqual(['Projects']);
  });
});
