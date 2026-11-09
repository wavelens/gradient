/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { checkWorkerUrlWithWebSocketOrHttpScheme } from './build-worker-url-field-check';

describe('checkWorkerUrlWithWebSocketOrHttpScheme', () => {
  it.each(['', 'wss://worker.example.com/proto', 'ws://10.0.0.2:3000/proto', 'https://worker.example.com', 'http://worker:8080'])(
    'accepts %s',
    (value) => expect(checkWorkerUrlWithWebSocketOrHttpScheme(value)).toBeNull(),
  );

  it.each(['worker.example.com', 'ftp://worker.example.com', 'not a url'])('names the allowed prefixes for %s', (value) =>
    expect(checkWorkerUrlWithWebSocketOrHttpScheme(value)).toContain('wss://'),
  );
});
