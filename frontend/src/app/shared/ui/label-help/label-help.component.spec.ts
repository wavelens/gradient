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

  it('links the docs page safely in a new tab', async () => {
    const a = await render({ doc: 'reference/wildcards/' });
    expect(a.getAttribute('href')).toBe('https://wavelens.github.io/gradient/reference/wildcards/');
    expect(a.getAttribute('target')).toBe('_blank');
    expect(a.getAttribute('rel')).toContain('noopener');
  });

  it('defaults its accessible name', async () => {
    expect((await render({ doc: 'reference/wildcards/' })).getAttribute('aria-label')).toBe('Learn more');
  });

  it('uses a custom title as the accessible name', async () => {
    const a = await render({ doc: 'reference/wildcards/', title: 'Naming rules' });
    expect(a.getAttribute('aria-label')).toBe('Naming rules');
  });

  it('describes the link by the field explanation', async () => {
    const a = await render({ doc: 'reference/wildcards/', title: 'Naming rules', explanationText: 'Output pattern to evaluate' });
    document.body.append(a.closest('gr-label-help') ?? a);
    expect(document.getElementById(a.getAttribute('aria-describedby')!)?.textContent).toBe('Output pattern to evaluate');
  });

  it('is a plain explanation button without a docs page', async () => {
    const fixture = TestBed.createComponent(LabelHelpComponent);
    fixture.componentRef.setInput('title', 'About Build time');
    fixture.componentRef.setInput('explanationText', 'Sum of all completed build durations');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('a')).toBeNull();
    expect(root.querySelector('button')?.getAttribute('aria-label')).toBe('About Build time');
  });
});
