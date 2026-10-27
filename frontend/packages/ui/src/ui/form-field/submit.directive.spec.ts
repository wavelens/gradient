/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ButtonComponent } from '../button/button.component';
import { SettingsSectionComponent } from '../settings-section/settings-section.component';
import { FormFieldComponent } from './form-field.component';
import { SubmitDirective } from './submit.directive';

@Component({
  standalone: true,
  imports: [SettingsSectionComponent, FormFieldComponent, ButtonComponent, SubmitDirective],
  template: `
    <gr-settings-section title="General">
      <gr-form-field label="URL" [invalid]="urlBad()" error="Bad URL"><input /></gr-form-field>
      <button grButton class="save" label="Save" (grSubmit)="saved = saved + 1"></button>
    </gr-settings-section>
    <gr-settings-section title="Other">
      <gr-form-field label="Name" [invalid]="nameBad()" error="Bad name"><input /></gr-form-field>
    </gr-settings-section>
  `,
})
class TwoSectionsHost {
  urlBad = signal(false);
  nameBad = signal(false);
  saved = 0;
}

describe('grSubmit', () => {
  async function render() {
    const fixture = TestBed.createComponent(TwoSectionsHost);
    fixture.detectChanges();
    await fixture.whenStable();
    const button = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.save')!;
    return { fixture, host: fixture.componentInstance, button };
  }

  it('submits while no field in its section shows an error', async () => {
    const { host, button } = await render();
    button.click();
    expect(host.saved).toBe(1);
    expect(button.getAttribute('aria-disabled')).toBeNull();
  });

  it('blocks the submit while a field in its section shows an error', async () => {
    const { fixture, host, button } = await render();
    host.urlBad.set(true);
    fixture.detectChanges();
    button.click();
    expect(host.saved).toBe(0);
    expect(button.getAttribute('aria-disabled')).toBe('true');

    host.urlBad.set(false);
    fixture.detectChanges();
    button.click();
    expect(host.saved).toBe(1);
  });

  it('ignores errors in another section', async () => {
    const { fixture, host, button } = await render();
    host.nameBad.set(true);
    fixture.detectChanges();
    button.click();
    expect(host.saved).toBe(1);
  });
});
