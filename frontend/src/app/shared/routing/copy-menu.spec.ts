/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { ConfigService } from '@core/services/config.service';
import { NotifyService } from '@core/services/notify.service';
import { CopyMenuService } from './copy-menu';
import { links } from './links';

describe('CopyMenuService', () => {
  function setup() {
    const notify = { copyToClipboard: vi.fn(() => Promise.resolve()) };
    TestBed.configureTestingModule({
      providers: [
        provideRouter([]),
        { provide: ConfigService, useValue: { apiBaseUrl: 'https://ci.example.com/api/v1' } },
        { provide: NotifyService, useValue: notify },
      ],
    });
    const item = TestBed.inject(CopyMenuService).createCopyMenuItemWithCliCommandApiUrlAndLink({
      gradientCliCommand: 'gradient evaluation wait acme/demo',
      relativeApiEndpointPath: 'tasks/acme/demo',
      absoluteLinkUrlTree: links.task('acme', 'demo'),
    });
    const run = (label: string) => item.submenuItems!.find((entry) => entry.label === label)!.command!();
    return { item, notify, run };
  }

  it('groups CLI command, API URL and Link under Copy', () => {
    const { item } = setup();
    expect(item.label).toBe('Copy');
    expect(item.submenuItems!.map((entry) => entry.label)).toEqual(['CLI command', 'API URL', 'Link']);
  });

  it('copies the CLI command, the absolute API URL and the page link through the shared clipboard toast', () => {
    const { run, notify } = setup();
    run('CLI command');
    run('API URL');
    run('Link');
    expect(notify.copyToClipboard.mock.calls).toEqual([
      ['gradient evaluation wait acme/demo', 'CLI command'],
      ['https://ci.example.com/api/v1/tasks/acme/demo', 'API URL'],
      [`${location.origin}/project/acme/task/demo`, 'link'],
    ]);
  });
});
