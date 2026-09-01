/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { IconComponent } from './icon.component';

@Component({
  standalone: true,
  imports: [IconComponent],
  template: `
    <gr-icon name="delete" />
    <gr-icon name="check" size="lg" label="Copied" />
  `,
})
class Host {}

describe('gr-icon', () => {
  async function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    return (fixture.nativeElement as HTMLElement).querySelectorAll('gr-icon');
  }

  it('renders the ligature name', async () => {
    expect((await render())[0].textContent?.trim()).toBe('delete');
  });

  it('carries the icon font class', async () => {
    expect((await render())[0].querySelector('.material-symbols-outlined')).not.toBeNull();
  });

  it('defaults to medium and applies the size class', async () => {
    const icons = await render();
    expect(icons[0].querySelector('.gr-icon--md')).not.toBeNull();
    expect(icons[1].querySelector('.gr-icon--lg')).not.toBeNull();
  });

  it('is decorative by default', async () => {
    const inner = (await render())[0].querySelector('span')!;
    expect(inner.getAttribute('aria-hidden')).toBe('true');
    expect(inner.hasAttribute('role')).toBe(false);
  });

  it('becomes a labelled image when given a label', async () => {
    const inner = (await render())[1].querySelector('span')!;
    expect(inner.getAttribute('role')).toBe('img');
    expect(inner.getAttribute('aria-label')).toBe('Copied');
    expect(inner.hasAttribute('aria-hidden')).toBe(false);
  });
});
