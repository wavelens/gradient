/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { CardGridComponent } from './card-grid.component';

@Component({
  standalone: true,
  imports: [CardGridComponent],
  template: `<gr-card-grid min="320px"><div class="card">a</div><div class="card">b</div></gr-card-grid>`,
})
class Host {}

describe('gr-card-grid', () => {
  it('projects its cards', async () => {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelectorAll('.card').length).toBe(2);
  });

  it('exposes the track size as a custom property', async () => {
    const fixture = TestBed.createComponent(CardGridComponent);
    fixture.componentRef.setInput('min', '320px');
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).style.getPropertyValue('--gr-card-min')).toBe('320px');
  });

  it('defaults the track size', async () => {
    const fixture = TestBed.createComponent(CardGridComponent);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).style.getPropertyValue('--gr-card-min')).toBe('400px');
  });
});
