/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { FormControl } from '@angular/forms';
import { PasswordInputComponent } from './password-input.component';

describe('PasswordInputComponent', () => {
  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [PasswordInputComponent] }).compileComponents();
  });

  it('toggles visibility on button click', async () => {
    const fixture = TestBed.createComponent(PasswordInputComponent);
    fixture.componentRef.setInput('control', new FormControl(''));
    fixture.detectChanges();
    await fixture.whenStable();

    const root = fixture.nativeElement as HTMLElement;
    const input = root.querySelector('input') as HTMLInputElement;
    const toggle = root.querySelector('.password-input__toggle') as HTMLButtonElement;
    expect(input.type).toBe('password');
    toggle.click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(input.type).toBe('text');
  });
});
