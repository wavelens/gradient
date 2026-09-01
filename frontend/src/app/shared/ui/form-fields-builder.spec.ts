/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { FormGroup } from '@angular/forms';
import { FormFieldsBuilder, passwordStrengthValidator } from './form-fields-builder';

describe('FormFieldsBuilder', () => {
  let ff: FormFieldsBuilder;

  beforeEach(() => {
    TestBed.configureTestingModule({});
    ff = TestBed.inject(FormFieldsBuilder);
  });

  it('creates required text fields', () => {
    const c = ff.text('', { required: true, minLength: 3 });
    expect(c.invalid).toBe(true);
    c.setValue('hi');
    expect(c.errors?.['minlength']).toBeDefined();
    c.setValue('hello');
    expect(c.valid).toBe(true);
  });

  it('creates email fields with email + required', () => {
    const c = ff.email();
    expect(c.errors?.['required']).toBe(true);
    c.setValue('not-an-email');
    expect(c.errors?.['email']).toBe(true);
    c.setValue('a@b.co');
    expect(c.valid).toBe(true);
  });

  it('creates password with strength validator', () => {
    const c = ff.password('', { required: true, strength: true });
    c.setValue('short');
    expect(c.errors?.['passwordStrength']).toBe(true);
    c.setValue('Strong1Pass');
    expect(c.valid).toBe(true);
  });

  it('confirm() validates against another control', () => {
    const group = new FormGroup({
      password: ff.password(''),
      confirm: ff.confirm('password'),
    });
    group.controls.password.setValue('abc');
    group.controls.confirm.setValue('xyz');
    expect(group.controls.confirm.errors?.['passwordMatch']).toBe(true);
    group.controls.confirm.setValue('abc');
    expect(group.controls.confirm.valid).toBe(true);
  });

  it('passwordStrengthValidator passes for null/empty', () => {
    const empty = new FormGroup({}).controls;
    void empty;
    expect(passwordStrengthValidator({ value: '' } as never)).toBeNull();
  });
});
