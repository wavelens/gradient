/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { CopyFieldComponent } from './copy-field.component';
import { FormFieldComponent } from '../form-field/form-field.component';

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

  it('lets the textarea grow with its content instead of clipping', async () => {
    const { root } = await render({ value, multiline: true });
    const ta = root.querySelector('textarea')!;
    expect(getComputedStyle(ta).height).not.toBe('34px');
  });

  it('names the copy button in the caller language', async () => {
    const { root } = await render({ value, copyLabel: 'In die Zwischenablage kopieren' });
    expect(root.querySelector('.copy-field__button')?.getAttribute('aria-label')).toBe(
      'In die Zwischenablage kopieren',
    );
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

  it('gives the id only to the inner control so a label names it', async () => {
    @Component({
      imports: [CopyFieldComponent],
      template: `<label for="token">Token</label><gr-copy-field inputId="token" value="secret" />`,
    })
    class Labelled {}
    const fixture = TestBed.createComponent(Labelled);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelectorAll('#token')).toHaveLength(1);
    expect(root.querySelector('label')?.control).toBe(root.querySelector('input'));
  });

  it('get name from label of surrounding form field', async () => {
    @Component({
      imports: [CopyFieldComponent, FormFieldComponent],
      template: `<gr-form-field label="Store address"><gr-copy-field value="ssh://host" /></gr-form-field>`,
    })
    class InField {}
    const fixture = TestBed.createComponent(InField);
    fixture.detectChanges();
    await fixture.whenStable();
    const input = (fixture.nativeElement as HTMLElement).querySelector('input');
    expect(input?.getAttribute('aria-label')).toBe('Store address');
  });

  it.each([
    { source: 'its own label', inputs: { value, label: 'Public key' }, name: 'Copy Public key' },
    { source: 'no label', inputs: { value }, name: 'Copy to clipboard' },
  ])('names the copy button after $source', async ({ inputs, name }) => {
    const { root } = await render(inputs);
    expect(root.querySelector('.copy-field__button')?.getAttribute('aria-label')).toBe(name);
  });

  it('names the copy button after the surrounding form field, also when a label names the input', async () => {
    @Component({
      imports: [CopyFieldComponent, FormFieldComponent],
      template: `<gr-form-field label="nix.conf snippet" for="conf"><gr-copy-field inputId="conf" value="substituters" /></gr-form-field>`,
    })
    class InFieldWithId {}
    const fixture = TestBed.createComponent(InFieldWithId);
    fixture.detectChanges();
    await fixture.whenStable();
    const button = (fixture.nativeElement as HTMLElement).querySelector('.copy-field__button');
    expect(button?.getAttribute('aria-label')).toBe('Copy nix.conf snippet');
  });

  it('copies the value and flags copied state', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const { fixture } = await render({ value });
    await fixture.componentInstance.copy();
    expect(writeText).toHaveBeenCalledWith(value);
    expect(fixture.componentInstance.copied()).toBe(true);
  });

  it('shows and announces Copied after copying', async () => {
    Object.defineProperty(navigator, 'clipboard', { value: { writeText: vi.fn().mockResolvedValue(undefined) }, configurable: true });
    const { fixture } = await render({ value });
    const status = (fixture.nativeElement as HTMLElement).querySelector('[aria-live="polite"]')!;
    expect(status.textContent?.trim()).toBe('');
    await fixture.componentInstance.copy();
    fixture.detectChanges();
    expect(status.textContent?.trim()).toBe('Copied');
  });
});
