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
  access = signal<AccessState>({ managed: false, canEdit: true });
}

describe('AccessBannerComponent', () => {
  let fixture: ComponentFixture<HostComponent>;

  function bannerEl(): HTMLElement | null {
    return fixture.nativeElement.querySelector('.access-banner');
  }

  beforeEach(() => {
    TestBed.configureTestingModule({ imports: [HostComponent] });
    fixture = TestBed.createComponent(HostComponent);
  });

  it('renders nothing for full access', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: true });
    fixture.detectChanges();
    expect(bannerEl()).toBeNull();
  });

  it('renders a managed banner when managed and canEdit', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: true });
    fixture.detectChanges();
    const el = bannerEl();
    expect(el).not.toBeNull();
    expect(el!.textContent).toMatch(/managed/i);
    expect(el!.classList).toContain('access-banner--managed');
  });

  it('renders a read-only banner when !canEdit and !managed', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: false });
    fixture.detectChanges();
    const el = bannerEl();
    expect(el).not.toBeNull();
    expect(el!.textContent).toMatch(/read-only/i);
    expect(el!.classList).toContain('access-banner--readonly');
  });

  it('renders a managed-readonly banner when both', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: false });
    fixture.detectChanges();
    const el = bannerEl();
    expect(el).not.toBeNull();
    expect(el!.classList).toContain('access-banner--managed-readonly');
  });
});
