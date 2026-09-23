/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { permissionLabel } from './permission-label';

describe('permissionLabel', () => {
  it('splits camel case into lowercase words', () => {
    expect(permissionLabel('TriggerEvaluation')).toBe('trigger evaluation');
    expect(permissionLabel('View')).toBe('view');
  });

  it('keeps an acronym together', () => {
    expect(permissionLabel('ManageSSHKeys')).toBe('manage ssh keys');
  });

  it('reads snake case the same way', () => {
    expect(permissionLabel('edit_task')).toBe('edit task');
  });
});
