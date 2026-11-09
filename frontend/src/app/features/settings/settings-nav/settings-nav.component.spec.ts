/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { ConfigService } from '@core/services/config.service';
import { PendingInvitesService } from '@core/services/pending-invites.service';
import { SettingsNavComponent } from './settings-nav.component';

function render(sshEnabled: boolean, pending = 0) {
  TestBed.configureTestingModule({
    imports: [SettingsNavComponent],
    providers: [
      provideRouter([]),
      { provide: ConfigService, useValue: { sshEnabled } },
      { provide: PendingInvitesService, useValue: { count: () => pending } },
    ],
  });
  const fixture = TestBed.createComponent(SettingsNavComponent);
  fixture.detectChanges();
  return Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('a')).map((a) => [a.textContent?.trim(), a.getAttribute('href')]);
}

describe('SettingsNavComponent', () => {
  it('links every settings page to its siblings and counts pending invites', () => {
    expect(render(true, 2)).toEqual([
      ['Profile', '/settings/profile'],
      ['API Keys', '/settings/keys'],
      ['SSH Keys', '/settings/ssh-keys'],
      ['Sessions', '/settings/sessions'],
      ['Invitations (2)', '/settings/invites'],
    ]);
  });

  it('leaves SSH keys out while the server has SSH turned off', () => {
    expect(render(false).map(([label]) => label)).not.toContain('SSH Keys');
  });
});
