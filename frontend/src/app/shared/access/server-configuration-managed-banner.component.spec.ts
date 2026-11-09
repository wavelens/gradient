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
    ['team', false],
    ['project', false],
  ])('names only what still works on a managed %s', (kind, mentioned) => {
    const fixture = TestBed.createComponent(ServerConfigurationManagedBannerComponent);
    fixture.componentRef.setInput('access', { managed: true, canEdit: true, canTrigger: true });
    fixture.componentRef.setInput('kind', kind);
    fixture.detectChanges();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text.includes('Deactivating the cache still works here.')).toBe(mentioned);
    expect(text).not.toContain('members');
  });

  it('shows the read-only note on managed settings to a read-only viewer', () => {
    expect(bannerText({ managed: true, canEdit: false, canTrigger: false }, true)).toContain('Only the admins of this project can change these settings.');
  });

  it('names the lowest built-in role that grants the required permission', () => {
    const fixture = TestBed.createComponent(ServerConfigurationManagedBannerComponent);
    fixture.componentRef.setInput('access', { managed: false, canEdit: false, canTrigger: false });
    fixture.componentRef.setInput('kind', 'task');
    fixture.componentRef.setInput('showReadOnlyNote', true);
    fixture.componentRef.setInput('permission', 'editTask');
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Only the members with Write or Admin on this project can change these settings.');
  });

  it('names the owning project admins to a read-only settings viewer', () => {
    const readOnly = { managed: false, canEdit: false, canTrigger: false };
    expect(bannerText(readOnly, true)).toContain('Only the admins of this project can change these settings.');
    expect(bannerText(readOnly)).toBe('');
    expect(bannerText({ managed: false, canEdit: true, canTrigger: true }, true)).toBe('');
  });
});
