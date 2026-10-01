/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { MESSAGE_BANNER_ROLE, MessageBannerComponent } from './message-banner.component';

describe('MessageBannerComponent', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [MessageBannerComponent] }).compileComponents();
  });

  it('is a status region unless the app owns the live regions', async () => {
    const banner = () => {
      const fixture = TestBed.createComponent(MessageBannerComponent);
      fixture.detectChanges();
      return (fixture.nativeElement as HTMLElement).querySelector('.message-banner');
    };
    expect(banner()?.getAttribute('role')).toBe('status');
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [MessageBannerComponent],
      providers: [{ provide: MESSAGE_BANNER_ROLE, useValue: null }],
    });
    expect(banner()?.hasAttribute('role')).toBe(false);
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
