/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { RegisterComponent } from './register.component';
import { USERNAME_RULE } from '../username-rule';
import { AuthService } from '@core/services/auth.service';
import { ConfigService } from '@core/services/config.service';
import { ApiError } from '@core/services/api.service';

const CHECK_DELAY_MS = 550;

function setup(checkUsername: (name: string) => Observable<unknown>) {
  const register = vi.fn(() => of('ok'));
  TestBed.configureTestingModule({
    imports: [RegisterComponent],
    providers: [
      provideRouter([]),
      { provide: AuthService, useValue: { checkUsername: vi.fn(checkUsername), register } },
      {
        provide: ConfigService,
        useValue: {
          passwordLoginEnabled: true,
          registrationDisabled: false,
          oidcEnabled: false,
          loginProviders: [],
          logoUrl: null,
          emailVerificationEnabled: false,
        },
      },
    ],
  });
  const fixture = TestBed.createComponent(RegisterComponent);
  fixture.detectChanges();
  const root = fixture.nativeElement as HTMLElement;
  return { fixture, root, register, check: TestBed.inject(AuthService).checkUsername as ReturnType<typeof vi.fn> };
}

async function typeUsername(fixture: ComponentFixture<RegisterComponent>, value: string): Promise<void> {
  const input = (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>('#username')!;
  input.value = value;
  input.dispatchEvent(new Event('input'));
  fixture.detectChanges();
  await new Promise((resolve) => setTimeout(resolve, CHECK_DELAY_MS));
  fixture.detectChanges();
}

const usernameField = (root: HTMLElement) => root.querySelector('#username')!.closest('gr-form-field')!;

describe('RegisterComponent - username check', () => {
  it('refuses a username with the rule the profile page uses', () => {
    const { fixture, root } = setup(() => of('Username is available'));
    const username = fixture.componentInstance.username!;
    username.setValue('-a');
    username.markAsTouched();
    fixture.detectChanges();
    expect(usernameField(root).textContent).toContain(USERNAME_RULE);
  });

  it('shows the reason for a taken name under the field and stops checking', async () => {
    const { fixture, root } = setup(() => throwError(() => new ApiError('Username is already taken', 200)));
    await typeUsername(fixture, 'alice');
    expect(fixture.componentInstance.username!.pending).toBe(false);
    expect(usernameField(root).textContent).toContain('Username is already taken');
    expect(usernameField(root).textContent).not.toContain('Checking availability');
  });

  it('offers a retry when the check itself failed and settles once it succeeds', async () => {
    let answer: () => Observable<unknown> = () => throwError(() => new ApiError('The server failed to process the request.', 500));
    const { fixture, root, check } = setup(() => answer());
    await typeUsername(fixture, 'carol');
    expect(fixture.componentInstance.username!.pending).toBe(false);
    expect(usernameField(root).textContent).toContain('Username Check Failed');

    answer = () => of('Username is available');
    Array.from(usernameField(root).querySelectorAll('button'))
      .find((b) => b.textContent?.includes('Retry'))!
      .click();
    await new Promise((resolve) => setTimeout(resolve, CHECK_DELAY_MS));
    fixture.detectChanges();
    expect(check).toHaveBeenCalledTimes(2);
    expect(fixture.componentInstance.username!.valid).toBe(true);
    expect(usernameField(root).textContent).not.toContain('Username Check Failed');
  });

  it('asks to wait when the server throttles the check', async () => {
    const { fixture, root } = setup(() => throwError(() => new ApiError('Too Many Requests', 429)));
    await typeUsername(fixture, 'dave');
    expect(usernameField(root).textContent).toContain('Too many checks. Wait a moment');
    expect(usernameField(root).textContent).not.toContain('Too Many Requests');
  });
});

describe('RegisterComponent - password rule', () => {
  it('accepts the special characters the server accepts and nothing else', () => {
    const { fixture } = setup(() => of('Username is available'));
    const password = fixture.componentInstance.password!;
    for (const accepted of ['Zebra7kq_', 'Zebra7kq-', 'Zebra7kq+', 'Zebra7kq=', 'Zebra7kq[', 'Zebra7kq;']) {
      password.setValue(accepted);
      expect(password.errors, accepted).toBeNull();
    }
    password.setValue('Zebra7kq"');
    expect(password.errors?.['passwordStrength']?.hasSpecialChar).toBe(false);
  });

  it('refuses the sequences, repeats and the word the server refuses', () => {
    const { fixture } = setup(() => of('Username is available'));
    const password = fixture.componentInstance.password!;
    const strength = (value: string) => {
      password.setValue(value);
      return password.errors?.['passwordStrength'];
    };
    expect(strength('Abcdefg1!x')?.hasFourConsecutiveAscendingCharacters).toBe(true);
    expect(strength('Zebra7kqqq!')?.passwordHasNoCharacterRepeatedThreeTimesInARow).toBe(false);
    expect(strength('MyPassword7!')?.passwordDoesNotContainPasswordWord).toBe(false);
  });
});

describe('RegisterComponent - submit', () => {
  it('shows every missing field instead of a silently disabled button', () => {
    const { fixture, root, register } = setup(() => of('Username is available'));
    const submit = root.querySelector<HTMLButtonElement>('button[type=submit]')!;
    expect(submit.disabled).toBe(false);
    submit.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('Username is required');
    expect(root.textContent).toContain('Full name is required');
    expect(root.textContent).toContain('Email is required');
    expect(root.textContent).toContain('Password is required');
    expect(register).not.toHaveBeenCalled();
  });

  it('shows an email taken by another account under the email field, not as a banner', async () => {
    const { fixture, root, register } = setup(() => of('Username is available'));
    register.mockReturnValueOnce(throwError(() => new ApiError('Email already exists', 409, 'email_address_already_in_use')));
    const component = fixture.componentInstance;
    component.registerForm.setValue({
      username: 'alice',
      name: 'Alice',
      email: 'alice@example.com',
      password: 'Zebra7kq_x',
      confirmPassword: 'Zebra7kq_x',
    });
    await new Promise((resolve) => setTimeout(resolve, CHECK_DELAY_MS));
    component.onSubmit();
    fixture.detectChanges();
    expect(register).toHaveBeenCalledOnce();
    expect(root.querySelector('#email')!.closest('gr-form-field')!.textContent).toContain('Email already exists');
    expect(component.errorMessage()).toBeNull();
  });
});
