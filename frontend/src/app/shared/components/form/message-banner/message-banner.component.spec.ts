/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { MessageBannerComponent } from './message-banner.component';

describe('MessageBannerComponent', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [MessageBannerComponent] }).compileComponents();
  });

  it('applies the type modifier class', async () => {
    const fixture = TestBed.createComponent(MessageBannerComponent);
    fixture.componentRef.setInput('type', 'error');
    fixture.detectChanges();
    await fixture.whenStable();
    const el = (fixture.nativeElement as HTMLElement).querySelector('.message-banner');
    expect(el?.classList.contains('message-banner--error')).toBe(true);
  });

  it('uses the default icon for the type', async () => {
    const fixture = TestBed.createComponent(MessageBannerComponent);
    fixture.componentRef.setInput('type', 'success');
    fixture.detectChanges();
    await fixture.whenStable();
    const icon = (fixture.nativeElement as HTMLElement).querySelector('.material-symbols-outlined');
    expect(icon?.textContent?.trim()).toBe('check_circle');
  });

  it('honors a custom icon override', async () => {
    const fixture = TestBed.createComponent(MessageBannerComponent);
    fixture.componentRef.setInput('type', 'info');
    fixture.componentRef.setInput('icon', 'lightbulb');
    fixture.detectChanges();
    await fixture.whenStable();
    const icon = (fixture.nativeElement as HTMLElement).querySelector('.material-symbols-outlined');
    expect(icon?.textContent?.trim()).toBe('lightbulb');
  });
});
