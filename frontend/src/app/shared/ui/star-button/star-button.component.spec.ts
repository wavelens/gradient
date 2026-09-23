/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of, throwError } from 'rxjs';
import { StarButtonComponent } from './star-button.component';
import { StarsService } from '@core/services/stars.service';

describe('StarButtonComponent', () => {
  it('toggles and tells the service the new state', () => {
    const set = vi.fn(() => of(true));
    TestBed.configureTestingModule({
      imports: [StarButtonComponent],
      providers: [{ provide: StarsService, useValue: { set } }],
    });
    const fixture = TestBed.createComponent(StarButtonComponent);
    fixture.componentRef.setInput('target', { kind: 'task', project: 'infra', task: 'hosts' });
    fixture.componentRef.setInput('starred', false);
    fixture.detectChanges();

    (fixture.nativeElement as HTMLElement).querySelector('button')!.click();
    fixture.detectChanges();

    expect(set).toHaveBeenCalledWith({ kind: 'task', project: 'infra', task: 'hosts' }, true);
    expect(fixture.componentInstance.starred()).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('button')!.getAttribute('aria-pressed')).toBe('true');
  });

  it('rolls back when the request fails', () => {
    const set = vi.fn(() => throwError(() => new Error('x')));
    TestBed.configureTestingModule({
      imports: [StarButtonComponent],
      providers: [{ provide: StarsService, useValue: { set } }],
    });
    const fixture = TestBed.createComponent(StarButtonComponent);
    fixture.componentRef.setInput('target', { kind: 'cache', cache: 'main' });
    fixture.componentRef.setInput('starred', true);
    fixture.detectChanges();

    (fixture.nativeElement as HTMLElement).querySelector('button')!.click();

    expect(fixture.componentInstance.starred()).toBe(true);
  });
});
