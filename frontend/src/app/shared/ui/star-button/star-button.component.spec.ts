/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Subject, of, throwError } from 'rxjs';
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
    expect((fixture.nativeElement as HTMLElement).querySelector('svg')!.classList).toContain('star--on');
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

    expect(set).toHaveBeenCalledWith({ kind: 'cache', cache: 'main' }, false);
    expect(fixture.componentInstance.starred()).toBe(true);
  });

  it('labels the button Star or Starred and fills the star when starred', () => {
    TestBed.configureTestingModule({
      imports: [StarButtonComponent],
      providers: [{ provide: StarsService, useValue: { set: () => of(true) } }],
    });
    const fixture = TestBed.createComponent(StarButtonComponent);
    fixture.componentRef.setInput('target', { kind: 'project', project: 'acme' });
    fixture.componentRef.setInput('labeled', true);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;

    expect(root.querySelector('button')!.textContent!.trim()).toBe('Star');
    expect(root.querySelector('svg')!.classList).not.toContain('star--on');

    root.querySelector('button')!.click();
    fixture.detectChanges();

    expect(root.querySelector('button')!.textContent!.trim()).toBe('Starred');
    expect(root.querySelector('svg')!.classList).toContain('star--on');
  });

  it('ignores clicks while a request is in flight and rolls back to the pre-click state', () => {
    const pending = new Subject<boolean>();
    const set = vi.fn(() => pending);
    TestBed.configureTestingModule({
      imports: [StarButtonComponent],
      providers: [{ provide: StarsService, useValue: { set } }],
    });
    const fixture = TestBed.createComponent(StarButtonComponent);
    fixture.componentRef.setInput('target', { kind: 'cache', cache: 'main' });
    fixture.detectChanges();
    const button = (fixture.nativeElement as HTMLElement).querySelector('button')!;

    button.click();
    button.click();
    expect(set).toHaveBeenCalledTimes(1);
    expect(fixture.componentInstance.starred()).toBe(true);

    pending.error(new Error('x'));
    expect(fixture.componentInstance.starred()).toBe(false);
    button.click();
    expect(set).toHaveBeenCalledTimes(2);
  });

  it('keeps the icon-only label fixed and reports the state through aria-pressed', () => {
    TestBed.configureTestingModule({
      imports: [StarButtonComponent],
      providers: [{ provide: StarsService, useValue: { set: () => of(true) } }],
    });
    const fixture = TestBed.createComponent(StarButtonComponent);
    fixture.componentRef.setInput('target', { kind: 'project', project: 'acme' });
    fixture.componentRef.setInput('starred', true);
    fixture.detectChanges();
    const button = (fixture.nativeElement as HTMLElement).querySelector('button')!;

    expect(button.getAttribute('aria-label')).toBe('Star');
    expect(button.getAttribute('aria-pressed')).toBe('true');
  });
});
