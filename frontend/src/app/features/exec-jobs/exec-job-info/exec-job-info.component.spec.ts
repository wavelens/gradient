/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import type { ExecJobResponse } from '@core/models';
import { ExecJobInfoComponent } from './exec-job-info.component';

const JOB = {
  status: 'queued',
  architecture: 'x86_64-linux',
  guest: { cpus: 4, memory_mb: 2048, disk_mb: 0 },
  nodes: 1,
  labels: ['3d-printer'],
  devices: [{ name: 'nvidia', count: 2 }],
  mounts: [],
  secrets: ['lab-key'],
  network: true,
  ssh: true,
  ports: [6006, 8080],
  image: null,
  timeout_secs: 5400,
  max_silent_secs: null,
  retries: 1,
  waiting_reason: {
    kind: 'exec_workers',
    unmet: { labels: [], devices: [], mounts: [], nodes: 0, custom_image: false, ssh: true, ports: [6006] },
    connected_workers: 2,
    matching_workers: 0,
  },
} as unknown as ExecJobResponse;

function render(job: ExecJobResponse): HTMLElement {
  TestBed.configureTestingModule({ imports: [ExecJobInfoComponent] });
  const fixture = TestBed.createComponent(ExecJobInfoComponent);
  fixture.componentRef.setInput('job', job);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

function rows(root: HTMLElement): Record<string, string> {
  return Object.fromEntries(Array.from(root.querySelectorAll('.requested tr')).map((row) => [
    row.querySelector('td.label')?.textContent?.trim() ?? '',
    Array.from(row.querySelectorAll('.values > span')).map((value) => value.textContent?.trim()).join(', '),
  ]));
}

describe('ExecJobInfoComponent', () => {
  it('lists every requested field and names unset ones', () => {
    expect(rows(render(JOB))).toEqual({
      Architecture: 'x86_64-linux',
      Guest: '4 CPUs · 2048 MB memory',
      Nodes: '1',
      Labels: '3d-printer',
      Devices: 'nvidia:2',
      Mounts: 'None',
      Secrets: 'lab-key',
      Network: 'Yes',
      SSH: 'Yes',
      Ports: '6006, 8080',
      Image: 'Default',
      Timeout: '1h 30m',
      'Time without output': 'Worker limit',
      Retries: '1',
    });
  });

  it('shows why the job waits and each missing worker feature', () => {
    const waiting = render(JOB).querySelector('.waiting');
    expect(waiting?.querySelector('gr-status-icon')).not.toBeNull();
    expect(waiting?.textContent).toContain('needs a worker that allows SSH, port 6006');
    expect(Array.from(waiting?.querySelectorAll('gr-badge') ?? []).map((badge) => badge.textContent?.trim()))
      .toEqual(['a worker that allows SSH', 'port 6006']);
  });

  it('shows no wait section for a job that does not wait', () => {
    expect(render({ ...JOB, status: 'running', waiting_reason: null }).querySelector('.waiting')).toBeNull();
  });
});
