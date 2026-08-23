/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { TooltipDirective } from './tooltip.directive';

@Component({
  standalone: true,
  imports: [TooltipDirective],
  template: `<button class="target" [grTooltip]="text()" tooltipPosition="bottom">Run</button>`,
})
class HostComponent {
  text = signal('Re-run this evaluation');
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  return { fixture, target: () => fixture.nativeElement.querySelector('.target') as HTMLElement };
}

const tip = () => document.querySelector('.gr-tooltip');

describe('TooltipDirective', () => {
  it('shows the text on hover and hides it again on leave', () => {
    const { fixture, target } = render();
    expect(tip()).toBeNull();
    target().dispatchEvent(new MouseEvent('mouseenter'));
    fixture.detectChanges();
    expect(tip()!.textContent).toContain('Re-run this evaluation');
    target().dispatchEvent(new MouseEvent('mouseleave'));
    fixture.detectChanges();
    expect(tip()).toBeNull();
  });

  it('also opens on keyboard focus', () => {
    const { fixture, target } = render();
    target().dispatchEvent(new FocusEvent('focus'));
    fixture.detectChanges();
    expect(tip()).toBeTruthy();
  });

  it('stays away when there is no text', () => {
    const { fixture, target } = render();
    fixture.componentInstance.text.set('');
    fixture.detectChanges();
    target().dispatchEvent(new MouseEvent('mouseenter'));
    fixture.detectChanges();
    expect(tip()).toBeNull();
  });

  it('does not stack overlays when hover fires twice', () => {
    const { fixture, target } = render();
    target().dispatchEvent(new MouseEvent('mouseenter'));
    target().dispatchEvent(new MouseEvent('mouseenter'));
    fixture.detectChanges();
    expect(document.querySelectorAll('.gr-tooltip')).toHaveLength(1);
  });

  it('is torn down with the host', () => {
    const { fixture, target } = render();
    target().dispatchEvent(new MouseEvent('mouseenter'));
    fixture.detectChanges();
    fixture.destroy();
    expect(tip()).toBeNull();
  });
});
