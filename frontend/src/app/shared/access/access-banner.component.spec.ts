/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed, ComponentFixture } from '@angular/core/testing';
import { AccessBannerComponent } from './access-banner.component';
import { AccessState } from '@core/models/access.model';

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [AccessBannerComponent],
  template: `<app-access-banner [access]="access()"></app-access-banner>`,
})
class HostComponent {
  access = signal<AccessState>({ managed: false, canEdit: true, canTrigger: true });
}

describe('AccessBannerComponent', () => {
  let fixture: ComponentFixture<HostComponent>;

  function bannerEl(): HTMLElement | null {
    return fixture.nativeElement.querySelector('gr-message-banner.access-banner');
  }

  beforeEach(() => {
    TestBed.configureTestingModule({ imports: [HostComponent] });
    fixture = TestBed.createComponent(HostComponent);
  });

  it('renders nothing for full access', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: true, canTrigger: true });
    fixture.detectChanges();
    expect(bannerEl()).toBeNull();
  });

  it('renders an info banner with the managed message when managed and canEdit', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: true, canTrigger: true });
    fixture.detectChanges();
    const el = bannerEl();
    expect(el).not.toBeNull();
    expect(el!.getAttribute('data-kind')).toBe('managed');
    expect(el!.textContent).toMatch(/managed/i);
    expect(el!.querySelector('.message-banner--info')).not.toBeNull();
  });

  it('renders an info banner with the read-only message when !canEdit and !managed', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: false, canTrigger: false });
    fixture.detectChanges();
    const el = bannerEl();
    expect(el).not.toBeNull();
    expect(el!.getAttribute('data-kind')).toBe('readonly');
    expect(el!.textContent).toMatch(/read-only/i);
    expect(el!.querySelector('.message-banner--info')).not.toBeNull();
  });

  it('renders an info banner when both managed and read-only', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: false, canTrigger: false });
    fixture.detectChanges();
    const el = bannerEl();
    expect(el).not.toBeNull();
    expect(el!.getAttribute('data-kind')).toBe('managed-readonly');
    expect(el!.querySelector('.message-banner--info')).not.toBeNull();
  });
});
