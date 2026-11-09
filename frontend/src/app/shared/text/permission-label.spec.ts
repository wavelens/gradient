/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { permissionLabel, permissionLabelWithCapitalizedFirstLetter } from './permission-label';

describe('permissionLabel', () => {
  it('splits camel case into lowercase words', () => {
    expect(permissionLabel('TriggerEvaluation')).toBe('trigger evaluation');
    expect(permissionLabel('View')).toBe('view');
  });

  it('keeps an acronym together and in upper case', () => {
    expect(permissionLabel('ManageSSHKeys')).toBe('manage SSH keys');
    expect(permissionLabel('manageSshKey')).toBe('manage SSH key');
  });

  it('reads snake case the same way', () => {
    expect(permissionLabel('edit_task')).toBe('edit task');
  });

  it('names both webhook permissions by the event notifications they manage', () => {
    expect(permissionLabel('manageWebhooks')).toBe('manage event notifications');
    expect(permissionLabel('manageCacheWebhooks')).toBe('manage event notifications');
  });

  it('capitalises the first letter for a picker option', () => {
    expect(permissionLabelWithCapitalizedFirstLetter('manageSshKey')).toBe('Manage SSH key');
    expect(permissionLabelWithCapitalizedFirstLetter('viewProject')).toBe('View project');
  });
});
