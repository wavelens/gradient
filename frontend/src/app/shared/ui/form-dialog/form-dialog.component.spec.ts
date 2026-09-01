/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { FormDialogComponent } from './form-dialog.component';

describe('gr-form-dialog', () => {
  async function open(inputs: Record<string, unknown> = {}) {
    const fixture = TestBed.createComponent(FormDialogComponent);
    fixture.componentRef.setInput('visible', true);
    for (const [k, v] of Object.entries(inputs)) fixture.componentRef.setInput(k, v);
    fixture.detectChanges();
    await fixture.whenStable();
    return { fixture, buttons: [...document.querySelectorAll('.gr-dialog-panel button[grButton]')] as HTMLButtonElement[] };
  }

  afterEach(() => {
    document.querySelectorAll('.cdk-overlay-container').forEach((n) => n.remove());
  });

  it('renders default cancel and submit labels', async () => {
    const { buttons } = await open();
    expect(buttons.map((b) => b.textContent?.trim())).toEqual(['Cancel', 'Save']);
  });

  it('uses custom labels', async () => {
    const { buttons } = await open({ cancelLabel: 'Back', submitLabel: 'Create' });
    expect(buttons.map((b) => b.textContent?.trim())).toEqual(['Back', 'Create']);
  });

  it('emits submit', async () => {
    const { fixture, buttons } = await open();
    let fired = 0;
    fixture.componentInstance.submit.subscribe(() => fired++);
    buttons[1].click();
    expect(fired).toBe(1);
  });

  it('closes itself on cancel and emits', async () => {
    const { fixture, buttons } = await open();
    let fired = 0;
    fixture.componentInstance.cancel.subscribe(() => fired++);
    buttons[0].click();
    expect(fired).toBe(1);
    expect(fixture.componentInstance.visible()).toBe(false);
  });

  it('disables both actions while loading', async () => {
    const { buttons } = await open({ loading: true });
    expect(buttons[0].disabled).toBe(true);
    expect(buttons[1].disabled).toBe(true);
  });
});
