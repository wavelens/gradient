/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal, viewChild } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { MenuComponent } from './menu.component';
import { MenuItem } from '../types';

@Component({
  standalone: true,
  imports: [MenuComponent],
  template: `
    <button class="anchor" (click)="menu().toggle($event)">Actions</button>
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
  return { fixture, anchor };
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
});
