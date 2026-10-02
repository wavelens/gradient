/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of } from 'rxjs';
import { BaseWorkersComponent } from './base-workers.component';
import { AdminService } from '@core/services/admin.service';
import { ConfigService } from '@core/services/config.service';
import { WorkersService } from '@core/services/workers.service';
import { BaseWorkerEntry } from '@core/models';

const stateWorker: BaseWorkerEntry = {
  worker_id: 'b1',
  display_name: 'Shared',
  url: null,
  enabled: true,
  auto_enable: true,
  gradient_ci: false,
  connected: true,
};
const gciWorker: BaseWorkerEntry = {
  worker_id: 'g1',
  display_name: 'Gradient.CI Servers',
  url: 'wss://servers.gradient.ci/proto',
  enabled: true,
  auto_enable: false,
  gradient_ci: true,
  connected: false,
  last_error: {
    reason: '403 base worker not enabled by any project',
    at: '2026-10-02T12:00:00',
    direction: 'outbound',
    before_auth: false,
  },
};

function setup(rows: BaseWorkerEntry[], offered = true): ComponentFixture<BaseWorkersComponent> {
  TestBed.configureTestingModule({
    imports: [BaseWorkersComponent],
    providers: [
      provideRouter([]),
      { provide: AdminService, useValue: { listBaseWorkers: vi.fn(() => of(rows)), deleteBaseWorker: vi.fn(() => of('ok')) } },
      { provide: ConfigService, useValue: { gradientCiEnabled: offered, gradientCiUrl: 'https://servers.gradient.ci' } },
      { provide: WorkersService, useValue: {} },
    ],
  });
  return TestBed.createComponent(BaseWorkersComponent);
}

async function settled(fixture: ComponentFixture<BaseWorkersComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

function buttonsLabelled(root: HTMLElement, label: string): HTMLButtonElement[] {
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).filter(
    (b) => b.querySelector('.gr-button__label')?.textContent?.trim() === label,
  );
}

describe('BaseWorkersComponent', () => {
  it('offers Connect base server while none is connected', async () => {
    const fixture = setup([stateWorker]);
    await settled(fixture);
    expect(buttonsLabelled(fixture.nativeElement, 'Connect base server').length).toBe(1);
  });

  it('hides Connect base server when the option is off', async () => {
    const fixture = setup([stateWorker], false);
    await settled(fixture);
    expect(buttonsLabelled(fixture.nativeElement, 'Connect base server').length).toBe(0);
  });

  it('offers Disconnect only on the Gradient.CI row and shows its offline reason', async () => {
    const fixture = setup([stateWorker, gciWorker]);
    await settled(fixture);
    expect(buttonsLabelled(fixture.nativeElement, 'Disconnect').length).toBe(1);
    expect(buttonsLabelled(fixture.nativeElement, 'Connect base server').length).toBe(0);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('403 base worker not enabled by any project');
  });
});
