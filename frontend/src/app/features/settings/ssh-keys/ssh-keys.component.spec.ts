/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { SshKeysComponent } from './ssh-keys.component';
import { UserService } from '@core/services/user.service';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { ApiError } from '@core/services/api.service';
import { SshKeyInfo } from '@core/models';

let notify = { success: vi.fn(), failure: vi.fn() };

const key: SshKeyInfo = {
  id: 'k1',
  name: 'laptop',
  fingerprint: 'SHA256:abc',
  created_at: '2026-10-02T00:00:00',
  last_used_at: null,
};

function setup(
  keys: SshKeyInfo[],
  overrides: Partial<Record<keyof UserService, unknown>> = {},
): ComponentFixture<SshKeysComponent> {
  notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [SshKeysComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: UserService, useValue: { getSshKeys: () => of(keys), ...overrides } },
      { provide: ConfigService, useValue: { sshEnabled: true, sshPort: 2222 } },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(SshKeysComponent);
  fixture.detectChanges();
  return fixture;
}

function buttonsByText(root: HTMLElement, text: string): HTMLButtonElement[] {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).filter((el) =>
    (el.textContent ?? '').trim().toLowerCase().includes(target),
  );
}

describe('SshKeysComponent', () => {
  it('shows a failed load with Try Again instead of No SSH keys', () => {
    const getSshKeys = vi.fn().mockReturnValueOnce(throwError(() => new Error('database is down'))).mockReturnValue(of([key]));
    const fixture = setup([], { getSshKeys });
    const root = fixture.nativeElement as HTMLElement;
    expect(root.textContent).toContain('Failed to load SSH keys');
    expect(root.textContent).not.toContain('No SSH keys');
    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(getSshKeys).toHaveBeenCalledTimes(2);
    expect(root.textContent).toContain('laptop');
  });

  it('lists keys with their fingerprint', () => {
    const fixture = setup([key]);
    const text = fixture.nativeElement.textContent;
    expect(text).toContain('laptop');
    expect(text).toContain('SHA256:abc');
  });

  it('shows the ssh-ng address and the port as ssh config', () => {
    const fixture = setup([]);
    const fields = Array.from(
      fixture.nativeElement.querySelectorAll('input, textarea'),
    ) as (HTMLInputElement | HTMLTextAreaElement)[];
    const values = fields.map((el) => el.value);
    const address = values.find((v) => v.startsWith('ssh-ng://'));
    expect(address).toBe(`ssh-ng://<project>@${location.hostname}`);
    expect(values.some((v) => v.includes('Port 2222') && v.includes('User <project>'))).toBe(true);
  });

  it('deletes a key by id', () => {
    const deleteSshKey = vi.fn(() => of('SSH key deleted'));
    const fixture = setup([key], { deleteSshKey });
    buttonsByText(fixture.nativeElement, 'delete')[0].click();
    expect(deleteSshKey).toHaveBeenCalledWith('k1');
    expect(notify.success).toHaveBeenCalledWith('Deleted SSH key');
  });

  it('reports a failed delete and enables Delete again', () => {
    const failure = new ApiError('database is down', 500);
    const fixture = setup([key], { deleteSshKey: () => throwError(() => failure) });
    buttonsByText(fixture.nativeElement, 'delete')[0].click();
    fixture.detectChanges();
    expect(notify.failure).toHaveBeenCalledWith('Failed to delete SSH key', failure);
    expect(buttonsByText(fixture.nativeElement, 'delete')[0].disabled).toBe(false);
  });

  it('keeps a refused key inside the add dialog', () => {
    const fixture = setup([], { addSshKey: () => throwError(() => new ApiError('Key already registered', 409)) });
    const component = fixture.componentInstance;
    component.openDialog();
    component.formName = 'laptop';
    component.formPublicKey = 'ssh-ed25519 AAAA';
    component.addKey();
    fixture.detectChanges();
    expect(component.showDialog()).toBe(true);
    expect(document.querySelector('.gr-dialog')?.textContent).toContain('Key already registered');
    expect((fixture.nativeElement as HTMLElement).textContent).not.toContain('Key already registered');
  });

  describe('Add SSH Key dialog', () => {
    async function openDialog(): Promise<ComponentFixture<SshKeysComponent>> {
      const fixture = setup([]);
      buttonsByText(fixture.nativeElement, 'add ssh key')[0].click();
      fixture.detectChanges();
      await fixture.whenStable();
      return fixture;
    }

    async function type(
      fixture: ComponentFixture<SshKeysComponent>,
      id: string,
      value: string,
    ): Promise<void> {
      const el = document.getElementById(id) as HTMLInputElement | HTMLTextAreaElement;
      el.value = value;
      el.dispatchEvent(new Event('input'));
      fixture.detectChanges();
      await fixture.whenStable();
    }

    const nameValue = () => (document.getElementById('ssh-key-name') as HTMLInputElement).value;

    it('fills the name from the comment of a pasted key', async () => {
      const fixture = await openDialog();
      await type(fixture, 'ssh-key-public', 'ssh-ed25519 AAAAC3Nza dennis@laptop\n');
      expect(nameValue()).toBe('dennis@laptop');
    });

    it('keeps a name the user typed', async () => {
      const fixture = await openDialog();
      await type(fixture, 'ssh-key-name', 'Work');
      await type(fixture, 'ssh-key-public', 'ssh-ed25519 AAAAC3Nza dennis@laptop');
      expect(nameValue()).toBe('Work');
    });

    it('leaves the name empty for a key without a comment', async () => {
      const fixture = await openDialog();
      await type(fixture, 'ssh-key-public', 'ssh-ed25519 AAAAC3Nza');
      expect(nameValue()).toBe('');
    });

    it('updates an auto-filled name when the key changes', async () => {
      const fixture = await openDialog();
      await type(fixture, 'ssh-key-public', 'ssh-ed25519 AAAAC3Nza dennis@laptop');
      await type(fixture, 'ssh-key-public', 'ssh-ed25519 AAAAC3Nza dennis@desktop');
      expect(nameValue()).toBe('dennis@desktop');
    });
  });
});
