/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { FieldRowComponent } from './field-row.component';

@Component({
  standalone: true,
  imports: [FieldRowComponent],
  template: `<gr-field-row label="Secret"><span class="chip">Configured</span></gr-field-row>`,
})
class SlotHost {}

describe('gr-field-row', () => {
  it('renders label and value', async () => {
    const fixture = TestBed.createComponent(FieldRowComponent);
    fixture.componentRef.setInput('label', 'Endpoint URL');
    fixture.componentRef.setInput('value', 'https://example.test/hook');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.field-row-label')?.textContent).toContain('Endpoint URL');
    expect(root.querySelector('.field-row-value')?.textContent).toContain('https://example.test/hook');
  });

  it('applies the mono treatment only when asked', async () => {
    const fixture = TestBed.createComponent(FieldRowComponent);
    fixture.componentRef.setInput('label', 'Hash');
    fixture.componentRef.setInput('value', 'sha256-abc');
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.is-mono')).toBeNull();
    fixture.componentRef.setInput('mono', true);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.is-mono')).not.toBeNull();
  });

  it('projects content when no value is given', async () => {
    const fixture = TestBed.createComponent(SlotHost);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.field-row-value .chip')).not.toBeNull();
  });
});
