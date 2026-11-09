/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { EMPTY, of, throwError } from 'rxjs';
import { NotifyService } from '@core/services/notify.service';
import { ConfirmService } from '@shared/confirm';
import {
  getMemberRemovalConsequenceSentence,
  injectSaveMemberRoleChangeWithOwnRoleConfirmation,
  newRoleLacksCurrentRolePermission,
} from './member-access-change-confirmation';

const roles = [
  { name: 'Admin', permissions: ['manageMembers', 'triggerEvaluation'] },
  { name: 'Write', permissions: ['triggerEvaluation'] },
  { name: 'releaser', permissions: ['triggerEvaluation', 'manageSecrets'] },
];

function setup() {
  const confirm = vi.fn(() => EMPTY);
  const notify = { failure: vi.fn() };
  TestBed.configureTestingModule({
    providers: [
      { provide: ConfirmService, useValue: { confirm } },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const save = TestBed.runInInjectionContext(() => injectSaveMemberRoleChangeWithOwnRoleConfirmation());
  return { save, confirm, notify };
}

describe('member access change confirmation', () => {
  it('treats a role as lower when it misses one current permission', () => {
    expect(newRoleLacksCurrentRolePermission(roles, 'Admin', 'Write')).toBe(true);
    expect(newRoleLacksCurrentRolePermission(roles, 'Write', 'Admin')).toBe(false);
    expect(newRoleLacksCurrentRolePermission(roles, 'Write', 'releaser')).toBe(false);
    expect(newRoleLacksCurrentRolePermission(roles, 'releaser', 'Admin')).toBe(true);
  });

  it('addresses the signed-in user in the second person on removal', () => {
    expect(getMemberRemovalConsequenceSentence('project', 'alice', true)).toBe('You lose your role on this project.');
    expect(getMemberRemovalConsequenceSentence('cache', 'bob', false)).toBe('bob loses their role on this cache.');
  });

  it('asks only before the signed-in user lowers their own role', () => {
    const { save, confirm } = setup();
    const request = vi.fn(() => of('ok'));
    save({ place: 'project', signedInUser: false, lowersAccess: true, target: 'bob', save: request }).subscribe();
    save({ place: 'project', signedInUser: true, lowersAccess: false, target: 'alice', save: request }).subscribe();
    expect(confirm).not.toHaveBeenCalled();
    expect(request).toHaveBeenCalledTimes(2);
    save({ place: 'project', signedInUser: true, lowersAccess: true, target: 'alice', save: request }).subscribe();
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'alice', consequence: 'You lose some permissions in this project.' }));
    expect(request).toHaveBeenCalledTimes(2);
  });

  it('reports a failed direct save as a toast', () => {
    const { save, notify } = setup();
    const failure = new Error('Forbidden');
    save({ place: 'team', signedInUser: false, lowersAccess: false, target: 'bob', save: () => throwError(() => failure) }).subscribe();
    expect(notify.failure).toHaveBeenCalledWith("Couldn't change the member's role", failure);
  });
});
