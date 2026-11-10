/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { MessageService } from '@gradient/ui/ui';
import { ApiError } from './api.service';
import { NotifyService } from './notify.service';

describe('NotifyService', () => {
  function setup() {
    vi.useFakeTimers();
    const notify = TestBed.inject(NotifyService);
    const messages = TestBed.inject(MessageService);
    return { notify, messages };
  }

  afterEach(() => vi.useRealTimers());

  it('reports a success as a short toast', () => {
    const { notify, messages } = setup();
    notify.success('Webhook deleted');
    expect(messages.messages()).toEqual([expect.objectContaining({ severity: 'success', summary: 'Webhook deleted' })]);
    vi.advanceTimersByTime(3000);
    expect(messages.messages()).toEqual([]);
  });

  it('keeps a failure on screen longer and shows the server reason', () => {
    const { notify, messages } = setup();
    notify.failure('Failed to delete webhook', new ApiError('Webhook is in use', 409));
    expect(messages.messages()[0]).toEqual(
      expect.objectContaining({ severity: 'error', summary: 'Failed to delete webhook', detail: 'Webhook is in use' }),
    );
    vi.advanceTimersByTime(3000);
    expect(messages.messages()).toHaveLength(1);
  });

  it('replaces a stale failure with a later success', () => {
    const { notify, messages } = setup();
    notify.failure("Couldn't save role", new ApiError('Conflict', 409));
    notify.success('Role saved');
    expect(messages.messages()).toEqual([expect.objectContaining({ severity: 'success', summary: 'Role saved' })]);
  });

  it('leaves an expired session to the sign-in redirect', () => {
    const { notify, messages } = setup();
    notify.failure("Couldn't start evaluation", new ApiError('Your session has ended', 401));
    expect(messages.messages()).toEqual([]);
  });

  it('never shows the text of a programming error', () => {
    const { notify, messages } = setup();
    notify.failure('Failed to delete webhook', new TypeError('x is undefined'));
    expect(messages.messages()[0].detail).toBeUndefined();
  });

  it('copies text and names the copied thing in the toast, also when the clipboard refuses', async () => {
    const { notify, messages } = setup();
    const writeText = vi.fn().mockResolvedValueOnce(undefined).mockRejectedValueOnce(new Error('denied'));
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    await notify.copyToClipboard('https://ci.example.com/project/acme', 'link');
    expect(writeText).toHaveBeenCalledWith('https://ci.example.com/project/acme');
    expect(messages.messages()).toEqual([expect.objectContaining({ severity: 'success', summary: 'Link copied' })]);
    await notify.copyToClipboard('gradient evaluation wait acme/demo', 'CLI command');
    expect(messages.messages()).toEqual([expect.objectContaining({ severity: 'error', summary: "Couldn't copy CLI command" })]);
  });
});
