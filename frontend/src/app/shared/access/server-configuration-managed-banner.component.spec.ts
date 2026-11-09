/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { AccessState } from '@core/models/access.model';
import { ServerConfigurationManagedBannerComponent } from './server-configuration-managed-banner.component';

describe('ServerConfigurationManagedBannerComponent', () => {
  function bannerText(access: AccessState): string {
    const fixture = TestBed.createComponent(ServerConfigurationManagedBannerComponent);
    fixture.componentRef.setInput('access', access);
    fixture.componentRef.setInput('kind', 'task');
    fixture.detectChanges();
    return (fixture.nativeElement as HTMLElement).textContent?.trim() ?? '';
  }

  it.each([
    [{ managed: true, canEdit: true, canTrigger: true }, true],
    [{ managed: true, canEdit: false, canTrigger: false }, false],
    [{ managed: false, canEdit: true, canTrigger: true }, false],
  ])('explains the server configuration only to someone who could edit otherwise: %o', (access, shown) => {
    expect(bannerText(access).includes('managed by the server configuration')).toBe(shown);
  });
});
