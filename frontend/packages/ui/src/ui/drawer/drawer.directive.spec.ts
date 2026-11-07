/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { DrawerDirective } from './drawer.directive';

@Component({
  standalone: true,
  imports: [DrawerDirective],
  template: `
    <button class="toggle" (click)="open.set(true)">Builds</button>
    <aside [grDrawer]="open()" (grDrawerClose)="open.set(false)"><a href="/x" class="first">First</a></aside>
  `,
})
class HostComponent {
  open = signal(false);
}

async function render() {
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  const root = fixture.nativeElement as HTMLElement;
  const toggle = root.querySelector<HTMLButtonElement>('.toggle')!;
  const first = root.querySelector<HTMLAnchorElement>('.first')!;
  const escape = () => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  const openDrawer = async () => {
    toggle.focus();
    toggle.click();
    fixture.detectChanges();
    await fixture.whenStable();
  };
  return { fixture, toggle, first, escape, openDrawer };
}

describe('DrawerDirective', () => {
  it('open state: focus on first focusable element in panel', async () => {
    const { first, openDrawer } = await render();
    await openDrawer();
    expect(document.activeElement).toBe(first);
  });

  it('escape keypresses emit close and restore focus to opener button', async () => {
    const { fixture, toggle, escape, openDrawer } = await render();
    await openDrawer();
    escape();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(false);
    expect(document.activeElement).toBe(toggle);
  });

  it('escape keypresses do nothing while closed', async () => {
    const { fixture, escape } = await render();
    const spy = vi.spyOn(fixture.componentInstance.open, 'set');
    escape();
    expect(spy).not.toHaveBeenCalled();
  });
});
