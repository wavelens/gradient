/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { AccessState } from '@core/models/access.model';
import { SettingsAccessStateBadgeComponent } from './settings-access-state-badge.component';

describe('SettingsAccessStateBadgeComponent', () => {
  const managedEditor: AccessState = { managed: true, canEdit: true, canTrigger: true };
  const managedViewer: AccessState = { managed: true, canEdit: false, canTrigger: false };
  const editor: AccessState = { managed: false, canEdit: true, canTrigger: true };
  const viewer: AccessState = { managed: false, canEdit: false, canTrigger: false };

  function render(access: AccessState | null, inputs: Record<string, unknown> = {}) {
    const fixture = TestBed.createComponent(SettingsAccessStateBadgeComponent);
    fixture.componentRef.setInput('access', access);
    fixture.componentRef.setInput('kind', 'task');
    for (const [name, value] of Object.entries(inputs)) fixture.componentRef.setInput(name, value);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    const chip = root.querySelector<HTMLButtonElement>('button.access-status-chip-button');
    const descriptionId = chip?.getAttribute('aria-describedby');
    return {
      chip,
      label: chip?.textContent?.replace('lock', '').trim() ?? null,
      hasLockIcon: !!chip?.querySelector('gr-icon'),
      tooltip: descriptionId ? root.querySelector(`[id="${descriptionId}"]`)?.textContent?.trim() : null,
    };
  }

  it.each([managedEditor, managedViewer])('marks managed settings with a lock and the server configuration reason: %o', (access) => {
    const { label, hasLockIcon, tooltip } = render(access, { showReadOnlyNote: true });
    expect(label).toBe('Managed');
    expect(hasLockIcon).toBe(true);
    expect(tooltip).toBe('Settings for this task come from the server configuration.');
  });

  it.each([
    ['cache', managedEditor, true],
    ['cache', managedViewer, false],
    ['team', managedEditor, false],
    ['project', managedEditor, false],
  ] as const)('tells only an editor of a managed %s that deactivation still works', (kind, access, mentioned) => {
    const { tooltip } = render(access, { kind });
    expect(tooltip?.endsWith('Deactivating the cache still works here.')).toBe(mentioned);
  });

  it('tells a read-only viewer who can change the settings, named by the lowest role with the permission', () => {
    const { label, hasLockIcon, tooltip } = render(viewer, { showReadOnlyNote: true, permission: 'editTask' });
    expect(label).toBe('Read-only');
    expect(hasLockIcon).toBe(false);
    expect(tooltip).toBe('Only the members with Write or Admin on this project can change these settings.');
    expect(render(viewer, { showReadOnlyNote: true }).tooltip).toBe('Only the admins of this project can change these settings.');
  });

  it.each([
    ['an editor of settings that are not managed', editor, true],
    ['a read-only viewer on a page without the read-only note', viewer, false],
    ['a page whose access is not known yet', null, true],
  ] as const)('shows nothing to %s', (_case, access, showReadOnlyNote) => {
    expect(render(access, { showReadOnlyNote }).chip).toBeNull();
  });

  it('is a button, so the tooltip opens on keyboard focus', () => {
    const { chip } = render(managedViewer);
    expect(chip?.type).toBe('button');
    expect(chip?.disabled).toBe(false);
  });
});
