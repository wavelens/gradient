/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { OverlayPositionBuilder } from '@angular/cdk/overlay';
import { Component, signal, viewChild } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { MenuComponent } from './menu.component';
import { MenuItem } from '../types';

@Component({
  standalone: true,
  imports: [MenuComponent],
  template: `
    <button class="anchor" (click)="menu().toggle($event)">Actions</button>
    <button class="other" (click)="menu().toggle($event)">Other</button>
    <div class="row" (contextmenu)="menu().openAt($event)">Row</div>
    <gr-menu [model]="model()"></gr-menu>
  `,
})
class HostComponent {
  menu = viewChild.required(MenuComponent);
  ran = signal(0);
  model = signal<MenuItem[]>([
    { label: 'Edit', icon: 'edit', command: () => this.ran.set(this.ran() + 1) },
    { separator: true },
    { label: 'Delete', icon: 'delete', disabled: true, danger: true },
    { label: 'Open', icon: 'open_in_new', routerLink: ['/elsewhere'] },
  ]);
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent], providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  const anchor = () => fixture.nativeElement.querySelector('.anchor') as HTMLButtonElement;
  const row = () => fixture.nativeElement.querySelector('.row') as HTMLElement;
  const other = () => fixture.nativeElement.querySelector('.other') as HTMLButtonElement;
  return { fixture, anchor, row, other };
}

const items = () => Array.from(document.querySelectorAll('.gr-menu__item')) as HTMLButtonElement[];

describe('MenuComponent', () => {
  it('opens on toggle and closes on a second toggle', () => {
    const { fixture, anchor } = render();
    expect(items()).toHaveLength(0);
    anchor().click();
    fixture.detectChanges();
    expect(items().map((i) => i.querySelector('span:last-child')!.textContent)).toEqual(['Edit', 'Delete', 'Open']);
    anchor().click();
    fixture.detectChanges();
    expect(items()).toHaveLength(0);
  });

  it('renders separators as their own role', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    expect(document.querySelectorAll('.gr-menu__separator')).toHaveLength(1);
  });

  it('executes the item command and closes', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    items()[0].click();
    fixture.detectChanges();
    expect(fixture.componentInstance.ran()).toBe(1);
    expect(items()).toHaveLength(0);
  });

  it('expands a submenu in place and runs a child command', () => {
    const { fixture, anchor } = render();
    const copied = vi.fn();
    fixture.componentInstance.model.set([
      { label: 'Copy', icon: 'content_copy', submenuItems: [{ label: 'Link', command: copied }] },
    ]);
    anchor().click();
    fixture.detectChanges();
    const parent = items()[0];
    expect(parent.getAttribute('aria-expanded')).toBe('false');
    expect(items()).toHaveLength(1);
    parent.click();
    fixture.detectChanges();
    expect(parent.getAttribute('aria-expanded')).toBe('true');
    expect(items()).toHaveLength(2);
    items()[1].click();
    fixture.detectChanges();
    expect(copied).toHaveBeenCalledTimes(1);
    expect(items()).toHaveLength(0);
  });

  it('marks destructive items as danger', () => {
    const { fixture, row } = render();
    fixture.componentInstance.menu().show(row());
    fixture.detectChanges();
    expect(items().map((i) => i.classList.contains('gr-menu__item--danger'))).toEqual([false, true, false]);
  });

  it('disables the items that ask for it', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    expect(items()[1].disabled).toBe(true);
  });

  it('keeps the page click working: another button opens its menu at once', () => {
    const spy = vi.spyOn(OverlayPositionBuilder.prototype, 'flexibleConnectedTo');
    const { fixture, anchor, other } = render();
    anchor().click();
    fixture.detectChanges();
    other().click();
    fixture.detectChanges();
    expect(items()).toHaveLength(3);
    expect(fixture.componentInstance.menu().isOpen()).toBe(true);
    expect(spy).toHaveBeenLastCalledWith(other());
  });

  it('closes on a click on the button that opened it and stays closed', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    anchor().click();
    fixture.detectChanges();
    expect(items()).toHaveLength(0);
    expect(fixture.componentInstance.menu().isOpen()).toBe(false);
  });

  it('closes on a click outside the menu', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    document.body.click();
    fixture.detectChanges();
    expect(items()).toHaveLength(0);
    expect(fixture.componentInstance.menu().isOpen()).toBe(false);
  });

  it('explains a disabled item through a tooltip and a hidden description only', async () => {
    const { fixture, anchor } = render();
    fixture.componentInstance.model.set([{ label: 'Artefacts', disabledReason: 'Needs a finished build' }]);
    anchor().click();
    fixture.detectChanges();
    await fixture.whenStable();
    const [blocked] = items();
    expect(document.querySelector('.gr-menu__reason')).toBeNull();
    expect(blocked.getAttribute('aria-disabled')).toBe('true');
    const reason = document.getElementById(blocked.getAttribute('aria-describedby')!)!;
    expect(reason.textContent).toBe('Needs a finished build');
    expect(reason.classList.contains('sr-only')).toBe(true);
  });

  it('stops keyboard focus on an item with a disabled reason, show nothing for it and ignore clicks on it', async () => {
    const { fixture, anchor } = render();
    const opened = vi.fn();
    fixture.componentInstance.model.set([
      { label: 'Artefacts', routerLink: ['/artefacts'], command: opened, disabledReason: 'Needs a finished build' },
      { label: 'Edit', command: () => undefined },
    ]);
    anchor().click();
    fixture.detectChanges();
    await fixture.whenStable();
    const [blocked, edit] = items();
    expect(blocked.tagName).toBe('BUTTON');
    expect(blocked.disabled).toBe(false);
    expect(blocked.getAttribute('aria-disabled')).toBe('true');
    const reason = document.getElementById(blocked.getAttribute('aria-describedby')!)!;
    expect(reason.textContent).toBe('Needs a finished build');
    expect(document.activeElement).toBe(blocked);
    press('ArrowDown');
    expect(document.activeElement).toBe(edit);
    press('ArrowDown');
    expect(document.activeElement).toBe(blocked);
    blocked.click();
    fixture.detectChanges();
    expect(opened).not.toHaveBeenCalled();
    expect(items()).toHaveLength(2);
  });

  it('Click an item through the full pointer sequence with the reason for the disabled item above it.', async () => {
    const { fixture, anchor } = render();
    const graph = vi.fn();
    fixture.componentInstance.model.set([
      { label: 'Artefacts', disabledReason: 'Needs a finished build' },
      { label: 'Dependency graph', command: graph },
    ]);
    anchor().click();
    fixture.detectChanges();
    await fixture.whenStable();
    const [blocked, target] = items();
    blocked.focus();
    const pointer = { bubbles: true, cancelable: true, button: 0 };
    target.dispatchEvent(new PointerEvent('pointerdown', pointer));
    target.dispatchEvent(new MouseEvent('mousedown', pointer));
    target.focus();
    fixture.detectChanges();
    target.dispatchEvent(new PointerEvent('pointerup', pointer));
    target.dispatchEvent(new MouseEvent('mouseup', pointer));
    target.dispatchEvent(new MouseEvent('click', pointer));
    fixture.detectChanges();
    expect(graph).toHaveBeenCalledTimes(1);
    expect(items()).toHaveLength(0);
  });

  /// A middle click opens the link in a new tab; the menu has done its job.
  it('renders links as anchors and closes after a middle click', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    const link = items()[2] as unknown as HTMLAnchorElement;
    expect(link.getAttribute('href')).toBe('/elsewhere');
    link.dispatchEvent(new MouseEvent('auxclick', { button: 1, bubbles: true }));
    fixture.detectChanges();
    expect(items()).toHaveLength(0);
  });

  const press = (key: string) => document.body.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));

  it('can move focus into panel and use arrow keys to move between items', async () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.menu().isOpen()).toBe(true);
    expect(document.activeElement).toBe(items()[0]);
    press('ArrowDown');
    expect(document.activeElement).toBe(items()[2]);
    press('ArrowDown');
    expect(document.activeElement).toBe(items()[0]);
    press('ArrowUp');
    expect(document.activeElement).toBe(items()[2]);
    press('Home');
    expect(document.activeElement).toBe(items()[0]);
  });

  it('can close on Escape and return focus to the button that opened it', () => {
    const { fixture, anchor } = render();
    anchor().focus();
    anchor().click();
    fixture.detectChanges();
    press('Escape');
    fixture.detectChanges();
    expect(items()).toHaveLength(0);
    expect(fixture.componentInstance.menu().isOpen()).toBe(false);
    expect(document.activeElement).toBe(anchor());
  });

  describe('openAt', () => {
    function rightClick(target: HTMLElement, x: number, y: number): MouseEvent {
      const event = new MouseEvent('contextmenu', { clientX: x, clientY: y, bubbles: true, cancelable: true });
      target.dispatchEvent(event);
      return event;
    }

    // jsdom reports a 0x0 viewport, so the rendered offsets are meaningless -
    // assert the origin handed to the position strategy instead.
    const origin = () => vi.spyOn(OverlayPositionBuilder.prototype, 'flexibleConnectedTo');

    it('anchors the panel at the pointer and suppresses the native menu', () => {
      const spy = origin();
      const { fixture, row } = render();
      const event = rightClick(row(), 120, 80);
      fixture.detectChanges();
      expect(event.defaultPrevented).toBe(true);
      expect(items()).toHaveLength(3);
      expect(spy).toHaveBeenCalledWith({ x: 120, y: 80 });
    });

    it('stays open when the right mouse button of the opening right click is released', () => {
      const { fixture, row } = render();
      rightClick(row(), 120, 80);
      row().dispatchEvent(new MouseEvent('auxclick', { button: 2, bubbles: true }));
      fixture.detectChanges();
      expect(items()).toHaveLength(3);
    });

    it('falls back to the event target when the Menu key fires without coordinates', () => {
      const spy = origin();
      const { fixture, row } = render();
      rightClick(row(), 0, 0);
      fixture.detectChanges();
      expect(spy).toHaveBeenCalledWith(row());
    });

    it('replaces an already open panel rather than stacking a second one', () => {
      const { fixture, row } = render();
      rightClick(row(), 10, 10);
      fixture.detectChanges();
      rightClick(row(), 40, 40);
      fixture.detectChanges();
      expect(document.querySelectorAll('.gr-menu')).toHaveLength(1);
    });
  });
});
