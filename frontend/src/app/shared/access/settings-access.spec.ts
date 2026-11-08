/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { canOpenProjectSettings, canOpenTaskSettings } from './settings-access';

describe('settings access', () => {
  it('offers project settings only to roles that can change something there', () => {
    expect(canOpenProjectSettings({ permissions: ['viewProject', 'triggerEvaluation'] })).toBe(false);
    expect(canOpenProjectSettings({ permissions: ['viewProject', 'manageWebhooks'] })).toBe(true);
    expect(canOpenProjectSettings(undefined)).toBe(false);
  });

  it('offers task settings only to roles that can edit the task, its triggers or its actions', () => {
    expect(canOpenTaskSettings({ permissions: ['viewProject', 'manageMembers'] })).toBe(false);
    expect(canOpenTaskSettings({ permissions: ['manageTriggers'] })).toBe(true);
  });
});
