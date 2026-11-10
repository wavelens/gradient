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
  function setup(writeText: (text: string) => Promise<void>) {
    Object.defineProperty(navigator, 'clipboard', { value: { writeText: vi.fn(writeText) }, configurable: true });
    const notify = { success: vi.fn(), failure: vi.fn() };
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
    const run = async (label: string) => {
      item.submenuItems!.find((entry) => entry.label === label)!.command!();
      await Promise.resolve();
    };
    return { item, notify, run, writeText: navigator.clipboard.writeText as ReturnType<typeof vi.fn> };
  }

  it('groups CLI command, API URL and Link under Copy', () => {
    const { item } = setup(() => Promise.resolve());
    expect(item.label).toBe('Copy');
    expect(item.submenuItems!.map((entry) => entry.label)).toEqual(['CLI command', 'API URL', 'Link']);
  });

  it('copies the CLI command, the absolute API URL and the page link with a toast', async () => {
    const { run, writeText, notify } = setup(() => Promise.resolve());
    await run('CLI command');
    await run('API URL');
    await run('Link');
    expect(writeText.mock.calls.map(([text]) => text)).toEqual([
      'gradient evaluation wait acme/demo',
      'https://ci.example.com/api/v1/tasks/acme/demo',
      `${location.origin}/project/acme/task/demo`,
    ]);
    expect(notify.success.mock.calls.map(([summary]) => summary)).toEqual(['CLI command copied', 'API URL copied', 'Link copied']);
  });

  it('reports a refused clipboard write', async () => {
    const { run, notify } = setup(() => Promise.reject(new Error('denied')));
    await run('Link');
    await Promise.resolve();
    expect(notify.failure).toHaveBeenCalledWith("Couldn't copy to clipboard", expect.any(Error));
  });
});
