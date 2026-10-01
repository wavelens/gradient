/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { InputDirective } from './input.directive';

@Component({
  standalone: true,
  imports: [InputDirective],
  template: `<input grInput /><textarea grInput></textarea><select grInput></select><input class="bare" />`,
})
class Host {}

describe('grInput', () => {
  async function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    return fixture.nativeElement as HTMLElement;
  }

  it('stamps the styling hook on input, textarea and select', async () => {
    const root = await render();
    expect(root.querySelectorAll('.gr-input').length).toBe(3);
  });

  it('leaves untagged controls alone', async () => {
    expect((await render()).querySelector('.bare')?.classList.contains('gr-input')).toBe(false);
  });
});
