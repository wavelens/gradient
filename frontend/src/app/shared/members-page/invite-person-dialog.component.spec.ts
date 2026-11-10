/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { NotifyService } from '@core/services/notify.service';
import { UserService } from '@core/services/user.service';
import type { MemberPlace } from '@shared/access';
import { InvitePersonDialogComponent } from './invite-person-dialog.component';
import { toPeopleListRowFromAccessPerson, type PeopleListRow } from './access-reason-label';
import type { MembersPageRequests } from './members-page-requests';

const carolThroughTeam = toPeopleListRowFromAccessPerson({
  user: 'carol',
  user_display_name: 'Carol',
  role: 'View',
  permissions: [],
  reasons: [{ kind: 'team', team: { name: 'alice-team', display_name: 'Alice Team' }, role: 'View', applies: true, managed: false }],
});

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [InvitePersonDialogComponent],
  template: `<app-invite-person-dialog [(visible)]="visible" [kind]="kind" [requests]="requests" [roleOptions]="roleOptions" [rows]="rows" (personAddedOrInvited)="added = added + 1" />`,
})
class HostComponent {
  visible = true;
  kind: MemberPlace = 'project';
  rows: PeopleListRow[] = [carolThroughTeam];
  roleOptions = [{ label: 'View', value: 'View' }, { label: 'Write', value: 'Write' }];
  added = 0;
  requests = { addMember: vi.fn(() => of('ok')) } as unknown as MembersPageRequests & { addMember: ReturnType<typeof vi.fn> };
}

async function setup(superuser = false, kind: MemberPlace = 'project') {
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    providers: [
      { provide: AuthService, useValue: { user: signal({ username: 'alice', superuser }) } },
      { provide: UserService, useValue: { searchUsers: () => of([]) } },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.componentInstance.kind = kind;
  const settle = async () => {
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  };
  await settle();
  const typeUsername = async (value: string) => {
    const input = document.querySelector<HTMLInputElement>('#invite-person-user')!;
    input.value = value;
    input.dispatchEvent(new Event('input'));
    await settle();
  };
  const addNowRadio = () => document.querySelector<HTMLInputElement>('input[value="add-now"]')!;
  const chooseAddNow = async () => {
    addNowRadio().click();
    await settle();
  };
  const submit = async () => {
    Array.from(document.querySelectorAll<HTMLButtonElement>('[grDialogFooter] button')).at(-1)!.click();
    await settle();
  };
  const dialogText = () => document.querySelector('.gr-dialog')?.textContent ?? '';
  return { host: fixture.componentInstance, notify, typeUsername, addNowRadio, chooseAddNow, submit, dialogText };
}

describe('InvitePersonDialogComponent', () => {
  it('explains why Add now is off for someone without team access and sends an invitation', async () => {
    const { host, notify, typeUsername, addNowRadio, submit, dialogText } = await setup();
    await typeUsername('dave');
    expect(addNowRadio().disabled).toBe(true);
    expect(dialogText()).toContain("Those people don't have access to this project through a team yet.");
    expect(dialogText()).toContain('Send dave an invitation for View.');
    await submit();
    expect(host.requests.addMember).toHaveBeenCalledWith('dave', 'View', false);
    expect(notify.success).toHaveBeenCalledWith('Invitation sent');
    expect(host.visible).toBe(false);
    expect(host.added).toBe(1);
  });

  it('adds someone who reaches the project through a team right away', async () => {
    const { host, notify, typeUsername, addNowRadio, chooseAddNow, submit, dialogText } = await setup();
    await typeUsername('carol');
    expect(addNowRadio().disabled).toBe(false);
    await chooseAddNow();
    expect(dialogText()).toContain('carol gets View now.');
    await submit();
    expect(host.requests.addMember).toHaveBeenCalledWith('carol', 'View', true);
    expect(notify.success).toHaveBeenCalledWith('Member added');
  });

  it('falls back to an invitation when the typed username loses team access after Add now was picked', async () => {
    const { host, typeUsername, chooseAddNow, submit } = await setup();
    await typeUsername('carol');
    await chooseAddNow();
    await typeUsername('dave');
    await submit();
    expect(host.requests.addMember).toHaveBeenCalledWith('dave', 'View', false);
  });

  it('lets instance administrators add anyone to a team right away, but nobody else', async () => {
    const admin = await setup(true, 'team');
    await admin.typeUsername('dave');
    expect(admin.addNowRadio().disabled).toBe(false);
    TestBed.resetTestingModule();
    const member = await setup(false, 'team');
    await member.typeUsername('carol');
    expect(member.addNowRadio().disabled).toBe(true);
    expect(member.dialogText()).toContain('Only instance administrators can add people to a team right away.');
  });

  it('asks for a username before sending anything', async () => {
    const { host, submit, dialogText } = await setup();
    await submit();
    expect(dialogText()).toContain('Enter a username.');
    expect(host.requests.addMember).not.toHaveBeenCalled();
  });
});
