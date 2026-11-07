/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { LoadMoreComponent } from './load-more.component';

function render(shown: number, total: number) {
  const fixture = TestBed.createComponent(LoadMoreComponent);
  fixture.componentRef.setInput('shown', shown);
  fixture.componentRef.setInput('total', total);
  fixture.detectChanges();
  return fixture;
}

describe('LoadMoreComponent', () => {
  it('shows the count and asks for the next page', () => {
    const fixture = render(50, 75);
    const more = vi.fn();
    fixture.componentInstance.more.subscribe(more);
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Showing 50 of 75');
    root.querySelector('button')!.click();
    expect(more).toHaveBeenCalledTimes(1);
  });

  it('hides once every item is shown', () => {
    expect((render(75, 75).nativeElement as HTMLElement).textContent?.trim()).toBe('');
  });
});
