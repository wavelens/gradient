/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { LabelHelpComponent } from './label-help.component';

describe('gr-label-help', () => {
  async function render(inputs: Record<string, unknown>) {
    const fixture = TestBed.createComponent(LabelHelpComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return (fixture.nativeElement as HTMLElement).querySelector('a')!;
  }

  it('links out safely in a new tab', async () => {
    const a = await render({ href: 'https://docs.example/x' });
    expect(a.getAttribute('href')).toBe('https://docs.example/x');
    expect(a.getAttribute('target')).toBe('_blank');
    expect(a.getAttribute('rel')).toContain('noopener');
  });

  it('defaults its accessible name', async () => {
    expect((await render({ href: 'https://x.test' })).getAttribute('aria-label')).toBe('Learn more');
  });

  it('uses a custom title as the accessible name', async () => {
    const a = await render({ href: 'https://x.test', title: 'Naming rules' });
    expect(a.getAttribute('aria-label')).toBe('Naming rules');
  });
});
