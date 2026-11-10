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
    <aside [grDrawer]="open()" (grDrawerClose)="open.set(false)">
      <a href="/x" class="first">First</a>
      <button class="pick" (click)="open.set(false)">Pick</button>
    </aside>
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

  it('wraps Tab and Shift+Tab around the open panel and lets Tab go once closed', async () => {
    const { fixture, first, openDrawer } = await render();
    const pick = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.pick')!;
    const tab = (target: HTMLElement, shiftKey = false) => {
      const event = new KeyboardEvent('keydown', { key: 'Tab', shiftKey, bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      return event;
    };
    await openDrawer();
    pick.focus();
    expect(tab(pick).defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);
    expect(tab(first, true).defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(pick);
    fixture.componentInstance.open.set(false);
    fixture.detectChanges();
    pick.focus();
    expect(tab(pick).defaultPrevented).toBe(false);
  });

  it('returns focus to the opener when an action inside the panel closes it', async () => {
    const { fixture, toggle, openDrawer } = await render();
    await openDrawer();
    const pick = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.pick')!;
    pick.focus();
    pick.click();
    fixture.detectChanges();
    expect(document.activeElement).toBe(toggle);
  });

  it('stays open when a menu inside it already handled Escape', async () => {
    const { fixture, first, openDrawer } = await render();
    await openDrawer();
    first.addEventListener('keydown', (event) => event.preventDefault());
    first.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(true);
  });

  it('escape keypresses do nothing while closed', async () => {
    const { fixture, escape } = await render();
    const spy = vi.spyOn(fixture.componentInstance.open, 'set');
    escape();
    expect(spy).not.toHaveBeenCalled();
  });
});
