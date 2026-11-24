/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Observable, of, throwError } from 'rxjs';
import { AccessState } from '@core/models/access.model';
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { nameAvailability } from './name-availability';
import { RenameUrlNameComponent } from './rename-row-and-dialog.component';

const writable: AccessState = { managed: false, canEdit: true, canTrigger: true };
const managed: AccessState = { managed: true, canEdit: true, canTrigger: true };

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [RenameUrlNameComponent],
  template: `
    <app-rename-url-name
      noun="cache"
      currentUrlName="demo"
      entityUuidToKeepOnRename="0c6f1d2e-5b8a-4f3e-9d71-2a4b6c8e0f13"
      [access]="access"
      [names]="names"
      [sendRenameRequestToServer]="rename"
      [settingsFormHasUnsavedChanges]="settingsFormHasUnsavedChanges()"
      (urlNameRenameAccepted)="renamed.push($event)"
    />
  `,
})
class HostComponent {
  access = writable;
  settingsFormHasUnsavedChanges = signal(false);
  available = true;
  names = nameAvailability(() => of(this.available), ['reserved-name']);
  rename: (name: string) => Observable<unknown> = () => of('ok');
  renamed: string[] = [];
}

async function setup(configure: (host: HostComponent) => void = () => {}) {
  TestBed.configureTestingModule({});
  const fixture = TestBed.createComponent(HostComponent);
  configure(fixture.componentInstance);
  const settle = async () => {
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  };
  await settle();
  const open = async () => {
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('gr-row button')!.click();
    await settle();
  };
  const type = async (value: string) => {
    const input = document.querySelector<HTMLInputElement>('#cache-new-name')!;
    input.value = value;
    input.dispatchEvent(new Event('input'));
    await new Promise((resolve) => setTimeout(resolve, 450));
    await settle();
  };
  const confirmButton = () =>
    Array.from(document.querySelectorAll<HTMLButtonElement>('.gr-dialog__footer button')).find((b) => b.textContent?.includes('Rename'))!;
  const confirm = async () => {
    confirmButton().click();
    await settle();
  };
  return { fixture, host: fixture.componentInstance, open, type, confirm, confirmButton };
}

describe('RenameUrlNameComponent', () => {
  it('opens the dialog with the current name, the URL warning and Rename disabled', async () => {
    const { open, confirmButton } = await setup();
    await open();
    expect(document.querySelector<HTMLInputElement>('#cache-new-name')!.value).toBe('demo');
    expect(document.querySelector('.gr-dialog')!.textContent).toContain('stop working');
    expect(confirmButton().disabled).toBe(true);
  });

  it('sends the new name, closes the dialog and reports the new name', async () => {
    const rename = vi.fn(() => of('ok'));
    const { host, open, type, confirm } = await setup((h) => (h.rename = rename));
    const success = vi.spyOn(TestBed.inject(NotifyService), 'success');
    await open();
    await type('renamed');
    await confirm();
    expect(rename).toHaveBeenCalledWith('renamed');
    expect(host.renamed).toEqual(['renamed']);
    expect(success).toHaveBeenCalledWith('Cache renamed');
    expect(document.querySelector('#cache-new-name')).toBeNull();
  });

  it('shows a name conflict from the server as a field error and stays open', async () => {
    const { host, open, type, confirm } = await setup((h) => (h.rename = () => throwError(() => new ApiError('Name taken', 409))));
    await open();
    await type('renamed');
    await confirm();
    expect(host.renamed).toEqual([]);
    const field = document.querySelector('#cache-new-name')!.closest('gr-form-field')!;
    expect(field.textContent).toContain('This name is already taken.');
  });

  it('shows other server errors in the dialog', async () => {
    const { open, type, confirm } = await setup((h) => (h.rename = () => throwError(() => new ApiError('Server down', 500))));
    await open();
    await type('renamed');
    await confirm();
    expect(document.querySelector('.gr-dialog')!.textContent).toContain('Server down');
  });

  it('blocks invalid, reserved and taken names', async () => {
    const { host, open, type, confirmButton } = await setup();
    await open();
    await type('-bad');
    expect(confirmButton().disabled).toBe(true);
    await type('reserved-name');
    expect(confirmButton().disabled).toBe(true);
    host.available = false;
    await type('taken');
    expect(confirmButton().disabled).toBe(true);
  });

  it('disables Rename while the settings form has unsaved edits and asks to save or discard them', async () => {
    const { fixture, host } = await setup((h) => h.settingsFormHasUnsavedChanges.set(true));
    const element = fixture.nativeElement as HTMLElement;
    expect(element.querySelector<HTMLButtonElement>('gr-row button')!.disabled).toBe(true);
    expect(element.textContent).toContain('Save or discard the unsaved changes above');
    host.settingsFormHasUnsavedChanges.set(false);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(element.querySelector<HTMLButtonElement>('gr-row button')!.disabled).toBe(false);
    expect(element.textContent).not.toContain('Save or discard');
  });

  it('disables Rename for managed entities and points to the state file with the id', async () => {
    const { fixture } = await setup((h) => (h.access = managed));
    const element = fixture.nativeElement as HTMLElement;
    expect(element.querySelector<HTMLButtonElement>('button')!.disabled).toBe(true);
    expect(element.textContent).toContain('state file');
    expect(element.textContent).toContain('0c6f1d2e-5b8a-4f3e-9d71-2a4b6c8e0f13');
  });
});
