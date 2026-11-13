/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { WorkerCardComponent, WorkerCardGridComponent, WorkerCardWorkerInput } from './worker-card.component';

const offline: WorkerCardWorkerInput = {
  worker_id: 'w1',
  display_name: 'Builder',
  registered_at: '2026-01-01T00:00:00',
  active: true,
  gradient_ci: false,
  connected: false,
  enable_network: true,
  enable_secrets: false,
  enable_exec: false,
  enable_eval: true,
  enable_build: true,
};

const live = {
  capabilities: { network: true, secrets: true, exec: false, eval: true, build: true, federate: false },
  architectures: ['x86_64-linux', 'i686-linux'],
  system_features: ['kvm'],
  max_concurrent_builds: 32,
  assigned_job_count: 3,
  draining: false,
} as unknown as NonNullable<WorkerCardWorkerInput['live']>;

const lastError = { reason: 'connection refused', at: '2026-10-02T12:00:00', direction: 'outbound', before_auth: false } as const;

@Component({
  standalone: true,
  imports: [WorkerCardComponent, WorkerCardGridComponent],
  template: `
    <app-worker-card-list>
      <app-worker-card [worker]="worker()" [showWorkerLastConnectionErrorReason]="canEdit()">
        <span slot="source">Project worker</span>
        <button slot="actions">Metrics</button>
      </app-worker-card>
    </app-worker-card-list>
  `,
})
class Host {
  worker = signal<WorkerCardWorkerInput>(offline);
  canEdit = signal(true);
}

function render(worker: WorkerCardWorkerInput, canEdit = true) {
  const fixture = TestBed.createComponent(Host);
  fixture.componentInstance.worker.set(worker);
  fixture.componentInstance.canEdit.set(canEdit);
  fixture.detectChanges();
  const root = fixture.nativeElement as HTMLElement;
  const text = (selector: string) => root.querySelector(selector)?.textContent?.replace(/\s+/g, ' ').trim();
  return {
    fixture,
    root,
    status: () => Array.from(root.querySelectorAll('.worker-card__status-pills-cell gr-badge')).map((b) => b.textContent?.trim()),
    load: () => text('.job-count'),
    loadLabel: () => text('.worker-card__load-job-count-label'),
  };
}

describe('WorkerCardComponent', () => {
  it('shows a connected worker with its running jobs, architectures and projected slots', () => {
    const { root, status, load, loadLabel } = render({ ...offline, connected: true, live, url: 'https://builder.example' });
    expect(root.querySelector('.worker-name-cell')?.textContent).toContain('Builder');
    expect(root.querySelector('.worker-source')?.textContent).toContain('Project worker');
    expect(root.querySelector('.worker-archs')?.textContent).toContain('x86_64-linux, i686-linux');
    expect(root.querySelector('.worker-url')?.textContent).toContain('https://builder.example');
    expect(status()).toEqual(['Connected']);
    expect(load()).toBe('3 / 32');
    expect(loadLabel()).toBe('Running jobs');
    expect(root.querySelector('.worker-card-action-buttons-cell button')?.textContent).toBe('Metrics');
  });

  it('keeps the system features behind the info button', () => {
    const { fixture, root } = render({ ...offline, connected: true, live });
    expect(root.textContent).not.toContain('kvm');
    root.querySelector<HTMLButtonElement>('[aria-label="System features"]')!.click();
    fixture.detectChanges();
    expect(document.body.textContent).toContain('kvm');
  });

  it('shows the last connection error of an offline worker only to editors', () => {
    const editor = render({ ...offline, last_error: lastError });
    expect(editor.status()).toEqual(['Offline']);
    expect(editor.load()).toBe('–');
    expect(editor.loadLabel()).toBe('last error: connection refused');
    expect(editor.root.querySelector('app-worker-card')?.classList).toContain('is-offline');

    TestBed.resetTestingModule();
    expect(render({ ...offline, last_error: lastError }, false).loadLabel()).toBe('');
  });

  it('shows a deactivated worker as inactive without its jobs, also while it is connected', () => {
    const { status, load } = render({ ...offline, active: false, connected: true, live });
    expect(status()).toEqual(['Inactive']);
    expect(load()).toBe('–');
  });

  it('says a connected worker without live data takes no jobs of this project', () => {
    const { root, loadLabel } = render({ ...offline, connected: true, live: null });
    expect(loadLabel()).toBe('No jobs for this project');
    expect(root.querySelector('.worker-card__load-job-count-label')?.getAttribute('title')).toContain('project token');
  });

  it('adds the Draining badge next to the connection status', () => {
    const { status } = render({ ...offline, connected: true, live: { ...live, draining: true }, requested_by_admin: true });
    expect(status()).toEqual(['Connected', 'Draining']);
  });
});
