/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ScrollRegionDirective } from './scroll-region.directive';

@Component({
  standalone: true,
  imports: [ScrollRegionDirective],
  template: '<div grScrollRegion class="region"><span>content</span></div>',
})
class HostComponent {}

async function render(scrollWidth: number, clientWidth: number) {
  vi.spyOn(HTMLElement.prototype, 'scrollWidth', 'get').mockReturnValue(scrollWidth);
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(clientWidth);
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  return (fixture.nativeElement as HTMLElement).querySelector('.region')!;
}

describe('ScrollRegionDirective', () => {
  afterEach(() => vi.restoreAllMocks());

  it('add container to tab order only on content overflow', async () => {
    const region = await render(800, 300);
    expect(region.getAttribute('tabindex')).toBe('0');
  });

  it('keep container out of tab order without content overflow', async () => {
    const region = await render(300, 300);
    expect(region.hasAttribute('tabindex')).toBe(false);
  });
});
