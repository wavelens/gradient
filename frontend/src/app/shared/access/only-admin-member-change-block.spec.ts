/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { buildMemberSoleAdminDisabledReasonTextForUsername } from './only-admin-member-change-block';

describe('buildMemberSoleAdminDisabledReasonTextForUsername', () => {
  it('blocks only the single Admin', () => {
    const reason = buildMemberSoleAdminDisabledReasonTextForUsername('team', ['alice']);
    expect(reason('alice')).toBe('Set another member as Admin first. Every team needs at least one Admin.');
    expect(reason('bob')).toBe('');
  });

  it.each([[[]], [['alice', 'bob']]])('blocks nobody with admins %o', (admins) => {
    expect(buildMemberSoleAdminDisabledReasonTextForUsername('project', admins)('alice')).toBe('');
  });
});
