/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { AccessState } from '@core/models/access.model';
import { ServerConfigurationManagedBannerComponent } from './server-configuration-managed-banner.component';

describe('ServerConfigurationManagedBannerComponent', () => {
  function bannerText(access: AccessState, showReadOnlyNote = false): string {
    const fixture = TestBed.createComponent(ServerConfigurationManagedBannerComponent);
    fixture.componentRef.setInput('access', access);
    fixture.componentRef.setInput('kind', 'task');
    fixture.componentRef.setInput('showReadOnlyNote', showReadOnlyNote);
    fixture.detectChanges();
    return (fixture.nativeElement as HTMLElement).textContent?.trim() ?? '';
  }

  it.each([
    [{ managed: true, canEdit: true, canTrigger: true }, true],
    [{ managed: true, canEdit: false, canTrigger: false }, false],
    [{ managed: false, canEdit: true, canTrigger: true }, false],
  ])('explains the server configuration only to someone who could edit otherwise: %o', (access, shown) => {
    expect(bannerText(access).includes('come from the server configuration')).toBe(shown);
  });

  it.each([
    ['cache', true],
    ['team', true],
    ['project', false],
  ])('mentions what still works on a managed %s: %s', (kind, mentioned) => {
    const fixture = TestBed.createComponent(ServerConfigurationManagedBannerComponent);
    fixture.componentRef.setInput('access', { managed: true, canEdit: true, canTrigger: true });
    fixture.componentRef.setInput('kind', kind);
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent?.includes('Deactivating and adding extra members still work here.')).toBe(mentioned);
  });

  it('tells a read-only settings viewer to contact the owning project admin', () => {
    const readOnly = { managed: false, canEdit: false, canTrigger: false };
    expect(bannerText(readOnly, true)).toContain('Contact an admin for this project.');
    expect(bannerText(readOnly)).toBe('');
    expect(bannerText({ managed: false, canEdit: true, canTrigger: true }, true)).toBe('');
  });
});
