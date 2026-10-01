/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, viewChild } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { PopoverComponent } from './popover.component';

@Component({
  standalone: true,
  imports: [PopoverComponent],
  template: `
    <button class="anchor" (click)="pop().toggle($event)">Why?</button>
    <gr-popover><p class="detail">Scoring rules</p></gr-popover>
  `,
})
class HostComponent {
  pop = viewChild.required(PopoverComponent);
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  return { fixture, anchor: () => fixture.nativeElement.querySelector('.anchor') as HTMLButtonElement };
}

const panel = () => document.querySelector('.gr-popover');

describe('PopoverComponent', () => {
  it('projects its content only once opened', () => {
    const { fixture, anchor } = render();
    expect(panel()).toBeNull();
    anchor().click();
    fixture.detectChanges();
    expect(panel()!.querySelector('.detail')!.textContent).toContain('Scoring rules');
  });

  it('closes on a second toggle', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    anchor().click();
    fixture.detectChanges();
    expect(panel()).toBeNull();
  });

  it('re-projects its content when reopened', () => {
    const { fixture, anchor } = render();
    for (const _ of [0, 1]) {
      anchor().click();
      fixture.detectChanges();
      expect(panel()!.querySelector('.detail')).toBeTruthy();
      anchor().click();
      fixture.detectChanges();
    }
  });

  it('tears the overlay down with the host', () => {
    const { fixture, anchor } = render();
    anchor().click();
    fixture.detectChanges();
    fixture.destroy();
    expect(panel()).toBeNull();
  });
});
