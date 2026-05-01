/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { FormControl, Validators } from '@angular/forms';
import { FormErrorComponent } from './form-error.component';

describe('FormErrorComponent', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [FormErrorComponent] }).compileComponents();
  });

  it('renders nothing when control is untouched', async () => {
    const fixture = TestBed.createComponent(FormErrorComponent);
    const ctrl = new FormControl('', Validators.required);
    fixture.componentRef.setInput('control', ctrl);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.form-error')).toBeNull();
  });

  it('renders default message after touched', async () => {
    const fixture = TestBed.createComponent(FormErrorComponent);
    const ctrl = new FormControl('', Validators.required);
    ctrl.markAsTouched();
    fixture.componentRef.setInput('control', ctrl);
    fixture.detectChanges();
    await fixture.whenStable();
    const text = (fixture.nativeElement as HTMLElement).querySelector('.form-error')?.textContent;
    expect(text).toContain('required');
  });

  it('uses override message when provided', async () => {
    const fixture = TestBed.createComponent(FormErrorComponent);
    const ctrl = new FormControl('', Validators.required);
    ctrl.markAsTouched();
    fixture.componentRef.setInput('control', ctrl);
    fixture.componentRef.setInput('messages', { required: 'Custom required text.' });
    fixture.detectChanges();
    await fixture.whenStable();
    const text = (fixture.nativeElement as HTMLElement).querySelector('.form-error')?.textContent;
    expect(text).toContain('Custom required text.');
  });

  it('formats minlength with the required length', async () => {
    const fixture = TestBed.createComponent(FormErrorComponent);
    const ctrl = new FormControl('a', Validators.minLength(5));
    ctrl.markAsTouched();
    fixture.componentRef.setInput('control', ctrl);
    fixture.detectChanges();
    await fixture.whenStable();
    const text = (fixture.nativeElement as HTMLElement).querySelector('.form-error')?.textContent;
    expect(text).toContain('5');
  });
});
