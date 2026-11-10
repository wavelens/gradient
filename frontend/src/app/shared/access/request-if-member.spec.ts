/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { firstValueFrom } from 'rxjs';
import { isViewerCacheMember, requestIfCacheMember } from './request-if-member';

describe('isViewerCacheMember', () => {
  it('counts a cache role as membership', () => {
    expect(isViewerCacheMember({ role: 'View', permissions: ['viewCache'] })).toBe(true);
  });

  it('leaves out a reader through a subscribed project, who holds no cache role', () => {
    expect(isViewerCacheMember({ role: null, permissions: ['viewCache', 'readStore'] })).toBe(false);
  });

  it('asks the server nothing for a reader without a cache role', async () => {
    const request = vi.fn();
    await expect(firstValueFrom(requestIfCacheMember({ role: null, permissions: ['viewCache'] }, request))).rejects.toBeTruthy();
    expect(request).not.toHaveBeenCalled();
  });
});
