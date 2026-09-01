/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { EmptyStateComponent } from './empty-state.component';

describe('gr-empty-state', () => {
  async function render(inputs: Record<string, unknown>) {
    const fixture = TestBed.createComponent(EmptyStateComponent);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return { fixture, root: fixture.nativeElement as HTMLElement };
  }

  const base = { icon: 'inbox', title: 'No items yet' };

  it('renders icon and title', async () => {
    const { root } = await render(base);
    expect(root.textContent).toContain('No items yet');
    expect(root.querySelector('.material-symbols-outlined')?.textContent).toContain('inbox');
  });

  it('omits the message when not given', async () => {
    expect((await render(base)).root.querySelector('.empty-message')).toBeNull();
  });

  it('renders the action only when a label is given', async () => {
    expect((await render(base)).root.querySelector('button')).toBeNull();
    expect((await render({ ...base, actionLabel: 'Create' })).root.querySelector('button')).not.toBeNull();
  });

  it('uses the button primitive for its action', async () => {
    const { root } = await render({ ...base, actionLabel: 'Create' });
    expect(root.querySelector('button')?.classList.contains('gr-button')).toBe(true);
  });

  it('emits actionClick', async () => {
    const { fixture, root } = await render({ ...base, actionLabel: 'Create' });
    let fired = 0;
    fixture.componentInstance.actionClick.subscribe(() => fired++);
    root.querySelector('button')!.click();
    expect(fired).toBe(1);
  });
});
