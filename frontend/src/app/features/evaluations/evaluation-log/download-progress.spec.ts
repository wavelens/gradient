/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { downloadLabel, downloadRatio } from './download-progress';

describe('downloadRatio', () => {
  it('is the fetched share of the announced size', () => {
    expect(downloadRatio({ downloaded: 256, total: 1024 })).toBe(0.25);
  });

  it('is unknown without an announced size', () => {
    expect(downloadRatio({ downloaded: 256, total: null })).toBeNull();
    expect(downloadRatio({ downloaded: 0, total: 0 })).toBeNull();
  });

  it('never passes a whole', () => {
    expect(downloadRatio({ downloaded: 2048, total: 1024 })).toBe(1);
  });
});

describe('downloadLabel', () => {
  it('names both sizes when the total is known, the count alone otherwise', () => {
    expect(downloadLabel({ downloaded: 1536, total: 3 * 1024 * 1024 })).toBe('1.5 KiB / 3.0 MiB');
    expect(downloadLabel({ downloaded: 1536, total: null })).toBe('1.5 KiB');
  });
});
