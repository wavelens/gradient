/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import type { BuildProgress } from '@core/models';
import { BuildProgressComponent } from './build-progress.component';

function renderBuildProgressComponent(progress: BuildProgress, compact = false): HTMLElement {
  const fixture = TestBed.createComponent(BuildProgressComponent);
  fixture.componentRef.setInput('progress', progress);
  fixture.componentRef.setInput('compact', compact);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('BuildProgressComponent', () => {
  it('shows the phase, the percent done and the amounts of a download with known size', () => {
    const root = renderBuildProgressComponent({ phase: 'download', bytes_done: 256, bytes_total: 1024, paths_done: 1, paths_total: 4 });

    expect(root.querySelector('.bp-title')?.textContent).toBe('Downloading 25 %');
    expect(root.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('25');
    expect(root.querySelector('.bp-paths')?.textContent).toBe('1 / 4 paths');
    expect(root.querySelector('.bp-icon')).not.toBeNull();
  });

  it('omit percent for unknown size and show no icon in compact form', () => {
    const root = renderBuildProgressComponent({ phase: 'upload', bytes_done: 0, paths_done: 1 }, true);

    expect(root.querySelector('.bp-title')?.textContent).toBe('Uploading outputs');
    expect(root.querySelector('[role="progressbar"]')?.hasAttribute('aria-valuenow')).toBe(false);
    expect(root.querySelector('.bp-paths')).toBeNull();
    expect(root.querySelector('.bp-icon')).toBeNull();
  });
});
