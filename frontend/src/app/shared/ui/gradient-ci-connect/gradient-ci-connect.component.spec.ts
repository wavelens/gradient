/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Observable, of, throwError } from 'rxjs';
import { GradientCiConnectComponent } from './gradient-ci-connect.component';
import { ConfigService } from '@core/services/config.service';
import { WorkersService } from '@core/services/workers.service';
import { ConnectionStatus, GradientCiScope } from '@core/models';

type StatusOf = (workerId: string) => Observable<ConnectionStatus | undefined>;

function setup(scope: GradientCiScope, statusOf: StatusOf) {
  TestBed.configureTestingModule({
    imports: [GradientCiConnectComponent],
    providers: [
      { provide: ConfigService, useValue: { gradientCiUrl: 'https://servers.gradient.ci' } },
      { provide: WorkersService, useValue: { connectGradientCi: vi.fn(() => of({ worker_id: 'g1' })) } },
    ],
  });
  const fixture = TestBed.createComponent(GradientCiConnectComponent);
  fixture.componentRef.setInput('scope', scope);
  fixture.componentRef.setInput('label', 'ci.example.com');
  fixture.componentRef.setInput('statusOf', statusOf);
  fixture.componentRef.setInput('visible', true);
  fixture.detectChanges();
  const component = fixture.componentInstance;
  component.token = 'gci1_0199a0b1-c2d3-7e4f-8a6b-9c0d1e2f3a4b_s3cret';
  component.submit();
  return { fixture, component };
}

describe('GradientCiConnectComponent', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('keeps polling through a failed status request and gives up after 30 s', () => {
    let calls = 0;
    const statusOf = vi.fn(() => (++calls === 1 ? throwError(() => new Error('502')) : of({ connected: false })));
    const { component } = setup('project', statusOf);

    vi.advanceTimersByTime(6_000);
    expect(component.waiting()).toBe(true);
    expect(statusOf).toHaveBeenCalledTimes(2);

    vi.advanceTimersByTime(30_000);
    expect(component.waiting()).toBe(false);
    expect(component.errorMessage()).not.toBeNull();
  });

  it('stops polling when the dialog is closed from its header', () => {
    const statusOf = vi.fn(() => of({ connected: false }));
    const { fixture, component } = setup('project', statusOf);

    document.querySelector<HTMLButtonElement>('.gr-dialog__close')!.click();
    fixture.detectChanges();
    vi.advanceTimersByTime(10_000);

    expect(component.waiting()).toBe(false);
    expect(component.visible()).toBe(false);
    expect(statusOf).not.toHaveBeenCalled();
  });

  it('finishes a base connection with a notice instead of waiting for a dial', () => {
    const statusOf = vi.fn(() => of(undefined));
    const { component } = setup('base', statusOf);

    vi.advanceTimersByTime(40_000);

    expect(statusOf).not.toHaveBeenCalled();
    expect(component.waiting()).toBe(false);
    expect(component.errorMessage()).toBeNull();
    expect(component.notice()).not.toBeNull();
  });
});
