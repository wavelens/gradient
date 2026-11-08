/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { InteractivityChecker } from '@angular/cdk/a11y';
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

class LayoutFreeInteractivityChecker extends InteractivityChecker {
  override isVisible(): boolean {
    return true;
  }
}

async function render() {
  TestBed.configureTestingModule({ providers: [{ provide: InteractivityChecker, useClass: LayoutFreeInteractivityChecker }] });
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

  it('keeps Tab inside the open panel and releases it on close', async () => {
    const { fixture, openDrawer } = await render();
    const anchors = () => Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('.cdk-focus-trap-anchor'), (a) => a.getAttribute('tabindex'));
    await openDrawer();
    expect(anchors()).toEqual(['0', '0']);
    fixture.componentInstance.open.set(false);
    fixture.detectChanges();
    expect(anchors()).not.toContain('0');
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

  it('escape keypresses do nothing while closed', async () => {
    const { fixture, escape } = await render();
    const spy = vi.spyOn(fixture.componentInstance.open, 'set');
    escape();
    expect(spy).not.toHaveBeenCalled();
  });
});
