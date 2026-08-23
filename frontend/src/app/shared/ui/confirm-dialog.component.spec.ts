/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ConfirmDialogComponent } from './confirm-dialog.component';
import { ConfirmationService } from './confirmation.service';

function render() {
  TestBed.configureTestingModule({ imports: [ConfirmDialogComponent], providers: [ConfirmationService] });
  const fixture = TestBed.createComponent(ConfirmDialogComponent);
  fixture.detectChanges();
  return { fixture, service: TestBed.inject(ConfirmationService) };
}

const panel = () => document.querySelector('.gr-dialog');
const footerButtons = () =>
  Array.from(document.querySelectorAll('.gr-dialog__footer button')) as HTMLButtonElement[];

describe('ConfirmDialogComponent', () => {
  it('stays closed until something is confirmed', () => {
    render();
    expect(panel()).toBeNull();
  });

  it('shows the header, message and button labels from the request', () => {
    const { fixture, service } = render();
    service.confirm({
      message: 'Are you sure you want to delete this item?',
      header: 'Confirm Delete',
      acceptButtonProps: { label: 'Delete', severity: 'danger' },
      rejectButtonProps: { label: 'Cancel', severity: 'secondary' },
    });
    fixture.detectChanges();
    expect(panel()!.textContent).toContain('Confirm Delete');
    expect(panel()!.textContent).toContain('Are you sure you want to delete this item?');
    expect(footerButtons().map((b) => b.textContent!.trim())).toEqual(['Cancel', 'Delete']);
  });

  it('runs accept and closes', () => {
    const { fixture, service } = render();
    let accepted = 0;
    service.confirm({ message: 'sure?', accept: () => accepted++ });
    fixture.detectChanges();
    footerButtons()[1].click();
    fixture.detectChanges();
    expect(accepted).toBe(1);
    expect(panel()).toBeNull();
  });

  it('runs reject and closes', () => {
    const { fixture, service } = render();
    let rejected = 0;
    service.confirm({ message: 'sure?', reject: () => rejected++ });
    fixture.detectChanges();
    footerButtons()[0].click();
    fixture.detectChanges();
    expect(rejected).toBe(1);
    expect(panel()).toBeNull();
  });

  it('falls back to default labels', () => {
    const { fixture, service } = render();
    service.confirm({ message: 'sure?' });
    fixture.detectChanges();
    expect(footerButtons().map((b) => b.textContent!.trim())).toEqual(['Cancel', 'Yes']);
  });
});
