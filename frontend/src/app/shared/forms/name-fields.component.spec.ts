/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { NameFieldsComponent } from './name-fields.component';
import { nameAvailability } from './name-availability';

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [NameFieldsComponent],
  template: `<app-name-fields noun="team" [names]="names" [(displayName)]="form.displayName" [(name)]="form.name" (submitted)="submits = submits + 1" />`,
})
class HostComponent {
  names = nameAvailability((name) => of(name !== 'taken'));
  form = { displayName: '', name: '' };
  submits = 0;
}

async function setup() {
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  const root = fixture.nativeElement as HTMLElement;
  const displayInput = root.querySelector<HTMLInputElement>('#team-display-name')!;
  const nameInput = root.querySelector<HTMLInputElement>('#team-name')!;
  const type = async (input: HTMLInputElement, value: string) => {
    input.value = value;
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  };
  return { fixture, host: fixture.componentInstance, displayInput, nameInput, type };
}

describe('NameFieldsComponent', () => {
  it('fills the name from the display name', async () => {
    const { host, nameInput, displayInput, type } = await setup();
    await type(displayInput, 'My Team');
    expect(host.form).toEqual({ displayName: 'My Team', name: 'my-team' });
    expect(nameInput.value).toBe('my-team');
  });

  it('keeps a name the user typed when the display name changes', async () => {
    const { host, nameInput, displayInput, type } = await setup();
    await type(displayInput, 'My Team');
    await type(nameInput, 'platform');
    await type(displayInput, 'Platform Team');
    expect(host.form).toEqual({ displayName: 'Platform Team', name: 'platform' });
    expect(host.names.state()).toBe('checking');
  });

  it('submits on enter in either field', async () => {
    const { host, nameInput, displayInput } = await setup();
    displayInput.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    nameInput.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    expect(host.submits).toBe(2);
  });
});
