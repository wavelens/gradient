/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { vi } from 'vitest';
import { SshKeysComponent } from './ssh-keys.component';
import { UserService } from '@core/services/user.service';
import { ConfigService } from '@core/services/config.service';
import { SshKey } from '@core/models/user.model';

const key: SshKey = {
  id: 'k1',
  name: 'laptop',
  fingerprint: 'SHA256:abc',
  created_at: '2026-10-02T00:00:00',
  last_used_at: null,
};

function setup(
  keys: SshKey[],
  overrides: Partial<Record<keyof UserService, unknown>> = {},
): ComponentFixture<SshKeysComponent> {
  TestBed.configureTestingModule({
    imports: [SshKeysComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: UserService, useValue: { getSshKeys: () => of(keys), ...overrides } },
      { provide: ConfigService, useValue: { sshEnabled: true, sshPort: 2222 } },
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
    expect(values.some((v) => v.includes('Port 2222'))).toBe(true);
  });

  it('deletes a key by id', () => {
    const deleteSshKey = vi.fn(() => of('SSH key deleted'));
    const fixture = setup([key], { deleteSshKey });
    buttonsByText(fixture.nativeElement, 'delete')[0].click();
    expect(deleteSshKey).toHaveBeenCalledWith('k1');
  });
});
