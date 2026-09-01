/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { CopyFieldComponent } from './copy-field.component';

async function render(inputs: Record<string, unknown>) {
  const fixture = TestBed.createComponent(CopyFieldComponent);
  for (const [key, value] of Object.entries(inputs)) {
    fixture.componentRef.setInput(key, value);
  }
  fixture.detectChanges();
  await fixture.whenStable();
  return { fixture, root: fixture.nativeElement as HTMLElement };
}

describe('CopyFieldComponent', () => {
  const value = '/nix/store/9k3m1x0a4b2c-hello-2.12.1';

  it('renders a single-line input by default', async () => {
    const { root } = await render({ value });
    expect(root.querySelector('input')).not.toBeNull();
    expect(root.querySelector('textarea')).toBeNull();
  });

  it('renders a textarea when multiline', async () => {
    const { root } = await render({ value, multiline: true });
    expect(root.querySelector('textarea')?.value).toBe(value);
    expect(root.querySelector('input')).toBeNull();
  });

  it('honours the rows input when multiline', async () => {
    const { root } = await render({ value, multiline: true, rows: 6 });
    expect(root.querySelector('textarea')?.rows).toBe(6);
  });

  it('lets the textarea grow with its rows instead of clipping', async () => {
    const { root } = await render({ value, multiline: true, rows: 4 });
    const ta = root.querySelector('textarea')!;
    // A fixed control height would leave scrollHeight above clientHeight.
    expect(getComputedStyle(ta).height).not.toBe('34px');
  });

  it('places the copy button inside the field', async () => {
    const { root } = await render({ value });
    expect(root.querySelector('.copy-field__button')).not.toBeNull();
    expect(root.querySelector('.copy-field > .copy-field__button')).not.toBeNull();
  });

  it('renders an inline code variant without an input', async () => {
    const { root } = await render({ value, inline: true });
    expect(root.querySelector('code')?.textContent).toContain(value);
    expect(root.querySelector('input')).toBeNull();
  });

  it('copies the value and flags copied state', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const { fixture } = await render({ value });
    await fixture.componentInstance.copy();
    expect(writeText).toHaveBeenCalledWith(value);
    expect(fixture.componentInstance.copied()).toBe(true);
  });
});
