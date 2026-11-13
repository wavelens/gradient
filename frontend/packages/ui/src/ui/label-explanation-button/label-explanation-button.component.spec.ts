/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { LabelExplanationButtonComponent } from './label-explanation-button.component';

describe('gr-label-explanation-button', () => {
  async function render(inputs: Record<string, unknown>) {
    const fixture = TestBed.createComponent(LabelExplanationButtonComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    document.body.append(root);
    return root;
  }

  afterEach(() => document.querySelectorAll('.gr-tooltip').forEach((tooltip) => tooltip.remove()));

  it('is a focusable button named by its label and described by the explanation', async () => {
    const root = await render({ iconButtonAriaLabel: 'About Branch', explanationText: 'Leave empty to track HEAD.' });
    const button = root.querySelector('button')!;
    expect(button.type).toBe('button');
    expect(button.getAttribute('aria-label')).toBe('About Branch');
    const description = document.getElementById(button.getAttribute('aria-describedby')!);
    expect(description?.textContent).toBe('Leave empty to track HEAD.');
    expect(root.querySelector('a')).toBeNull();
  });

  it('shows the explanation as tooltip on keyboard focus', async () => {
    const root = await render({ iconButtonAriaLabel: 'About Branch', explanationText: 'Leave empty to track HEAD.' });
    root.querySelector('button')!.dispatchEvent(new FocusEvent('focus'));
    expect(document.querySelector('.gr-tooltip')?.textContent).toBe('Leave empty to track HEAD.');
  });

  it('becomes a docs link that opens a new tab and keeps the explanation as tooltip', async () => {
    const root = await render({
      iconButtonAriaLabel: 'Worker capabilities',
      explanationText: 'Changes apply on the next reconnect.',
      documentationPageUrl: 'https://docs.example/workers',
    });
    const link = root.querySelector('a')!;
    expect(root.querySelector('button')).toBeNull();
    expect(link.getAttribute('href')).toBe('https://docs.example/workers');
    expect(link.getAttribute('target')).toBe('_blank');
    expect(link.getAttribute('rel')).toContain('noopener');
    link.dispatchEvent(new MouseEvent('mouseenter'));
    expect(document.querySelector('.gr-tooltip')?.textContent).toBe('Changes apply on the next reconnect.');
  });

  it('falls back to its label as tooltip and adds no description without an explanation', async () => {
    const root = await render({ iconButtonAriaLabel: 'Trigger types', documentationPageUrl: 'https://docs.example/triggers' });
    const link = root.querySelector('a')!;
    expect(link.hasAttribute('aria-describedby')).toBe(false);
    link.dispatchEvent(new FocusEvent('focus'));
    expect(document.querySelector('.gr-tooltip')?.textContent).toBe('Trigger types');
  });
});
