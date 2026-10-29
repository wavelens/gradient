/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { vi } from 'vitest';
import type { ExecJobOutputView, ExecJobResponse, ExecJobStatus } from '@core/models';
import { ApiService } from '@core/services/api.service';
import { ExecJobOutputsComponent } from './exec-job-outputs.component';

const OUTPUTS: ExecJobOutputView[] = [
  {
    path: 'model/w.bin',
    size: 3_000_000,
    content_type: 'application/octet-stream',
    created_at: '2026-01-01T00:00:01',
    updated_at: '2026-01-01T00:00:02',
  },
  {
    path: 'gradient/tree.patch',
    size: 120,
    content_type: 'text/x-diff',
    created_at: '2026-01-01T00:00:03',
    updated_at: '2026-01-01T00:00:03',
  },
];

function job(status: ExecJobStatus): ExecJobResponse {
  return { id: 'j1', status, outputs_size: 3_000_120 } as ExecJobResponse;
}

function setup(status: ExecJobStatus): {
  fixture: ComponentFixture<ExecJobOutputsComponent>;
  outputs: ReturnType<typeof vi.fn>;
} {
  const outputs = vi.fn(() => of(OUTPUTS));
  TestBed.configureTestingModule({
    imports: [ExecJobOutputsComponent],
    providers: [{ provide: ApiService, useValue: { get: outputs } }],
  });
  const fixture = TestBed.createComponent(ExecJobOutputsComponent);
  fixture.componentRef.setInput('job', job(status));
  fixture.detectChanges();
  return { fixture, outputs };
}

describe('ExecJobOutputsComponent', () => {
  afterEach(() => vi.useRealTimers());

  it('lists each output with its size and a download link', () => {
    const { fixture } = setup('completed');
    const root = fixture.nativeElement as HTMLElement;
    const link = root.querySelector<HTMLAnchorElement>('a[href$="model/w.bin"]');
    expect(link?.getAttribute('href')).toBe('/api/v1/exec/j1/outputs/model/w.bin');
    expect(link?.textContent?.trim()).toBe('model/w.bin');
    expect(root.textContent).toContain('2.9 MiB');
    expect(root.textContent).toContain('120 B');
  });

  it('lists and links the outputs of the picked node', () => {
    const { fixture, outputs } = setup('completed');
    fixture.componentRef.setInput('node', 1);
    fixture.detectChanges();
    expect(outputs).toHaveBeenLastCalledWith('exec/j1/outputs?node=1');
    const link = (fixture.nativeElement as HTMLElement).querySelector<HTMLAnchorElement>('a');
    expect(link?.getAttribute('href')).toBe('/api/v1/exec/j1/outputs/model/w.bin?node=1');
  });

  it('polls again after 2 s while the job runs', () => {
    vi.useFakeTimers();
    const { fixture, outputs } = setup('running');
    expect(outputs).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(2000);
    fixture.detectChanges();
    expect(outputs).toHaveBeenCalledTimes(2);
  });

  it('stops polling once the job finished', () => {
    vi.useFakeTimers();
    const { fixture, outputs } = setup('completed');
    vi.advanceTimersByTime(6000);
    fixture.detectChanges();
    expect(outputs).toHaveBeenCalledTimes(1);
  });
});
