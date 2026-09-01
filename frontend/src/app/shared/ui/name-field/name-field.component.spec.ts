/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { NameCheckState, NameFieldComponent } from './name-field.component';

@Component({
  standalone: true,
  imports: [NameFieldComponent, FormsModule],
  template: `
    <gr-name-field
      inputId="name"
      label="Name"
      [state]="state()"
      [ngModel]="name()"
      (ngModelChange)="name.set($event)"
      name="name"
    />
  `,
})
class HostComponent {
  name = signal('');
  state = signal<NameCheckState>('idle');
}

async function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  const root = () => fixture.nativeElement as HTMLElement;
  const set = async (s: NameCheckState) => {
    fixture.componentInstance.state.set(s);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  };
  return { fixture, root, set };
}

describe('gr-name-field', () => {
  it('labels the input it owns', async () => {
    const { root } = await render();
    expect(root().querySelector('label')?.getAttribute('for')).toBe('name');
    expect(root().querySelector('input')?.id).toBe('name');
  });

  it('shows the hint while idle and no error', async () => {
    const { root } = await render();
    expect(root().querySelector('.field-error')).toBeNull();
    expect(root().textContent).toContain('Lowercase letters, numbers, and hyphens only');
  });

  it('explains a taken name', async () => {
    const { root, set } = await render();
    await set('taken');
    expect(root().querySelector('.field-error')?.textContent).toContain('already taken');
  });

  it('explains a reserved name', async () => {
    const { root, set } = await render();
    await set('reserved');
    expect(root().querySelector('.field-error')?.textContent).toContain('reserved');
  });

  it('explains an invalid name', async () => {
    const { root, set } = await render();
    await set('invalid');
    expect(root().querySelector('.field-error')?.textContent).toContain('Lowercase letters');
  });

  it('marks an available name without raising an error', async () => {
    const { root, set } = await render();
    await set('available');
    expect(root().querySelector('.name-field__status')?.classList).toContain('is-available');
    expect(root().querySelector('.field-error')).toBeNull();
  });

  it('spins while the check is in flight', async () => {
    const { root, set } = await render();
    await set('checking');
    expect(root().querySelector('.gr-spin')).not.toBeNull();
  });

  it('writes typed text back through ngModel', async () => {
    const { fixture, root } = await render();
    const input = root().querySelector('input') as HTMLInputElement;
    input.value = 'my-project';
    input.dispatchEvent(new Event('input'));
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.name()).toBe('my-project');
  });
});
