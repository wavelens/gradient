/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { OverlayPositionBuilder } from '@angular/cdk/overlay';
import { Component, signal, viewChild } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { MenuComponent } from './menu.component';
import { MenuItem } from '../types';

@Component({
  standalone: true,
  imports: [MenuComponent],
  template: `
    <button class="anchor" (click)="menu().toggle($event)">Actions</button>
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
    { label: 'Delete', icon: 'delete', disabled: true },
  ]);
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  const anchor = () => fixture.nativeElement.querySelector('.anchor') as HTMLButtonElement;
  const row = () => fixture.nativeElement.querySelector('.row') as HTMLElement;
  return { fixture, anchor, row };
}

const items = () => Array.from(document.querySelectorAll('.gr-menu__item')) as HTMLButtonElement[];

describe('MenuComponent', () => {
  it('opens on toggle and closes on a second toggle', () => {
    const { fixture, anchor } = render();
    expect(items()).toHaveLength(0);
    anchor().click();
    fixture.detectChanges();
    expect(items().map((i) => i.querySelector('span:last-child')!.textContent)).toEqual(['Edit', 'Delete']);
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

  it('runs the item command and closes', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    items()[0].click();
    fixture.detectChanges();
    expect(fixture.componentInstance.ran()).toBe(1);
    expect(items()).toHaveLength(0);
  });

  it('disables the items that ask for it', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    expect(items()[1].disabled).toBe(true);
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
      expect(items()).toHaveLength(2);
      expect(spy).toHaveBeenCalledWith({ x: 120, y: 80 });
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
