/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injector, runInInjectionContext } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { EMPTY, Observable, firstValueFrom, isObservable } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { dialogFormChangesSinceOpen, unsavedChanges } from './unsaved-changes';

function setup(answer: 'leave' | 'stay') {
  const confirm = vi.fn((request: ConfirmRequest) => (answer === 'leave' ? request.action() : EMPTY));
  TestBed.configureTestingModule({ providers: [{ provide: ConfirmService, useValue: { confirm } }] });
  const form = { name: 'Acme' };
  const tracker = runInInjectionContext(TestBed.inject(Injector), () => unsavedChanges(() => form, 'Project settings'));
  return { form, tracker, confirm };
}

describe('unsavedChanges', () => {
  it('lets the user leave freely before loading and when nothing changed', () => {
    const { tracker, confirm } = setup('stay');
    expect(tracker.canLeave()).toBe(true);
    tracker.mark();
    expect(tracker.changed()).toBe(false);
    expect(tracker.canLeave()).toBe(true);
    expect(confirm).not.toHaveBeenCalled();
  });

  it('asks before leaving with edits and stays when the user cancels', async () => {
    const { form, tracker, confirm } = setup('stay');
    tracker.mark();
    form.name = 'Acme Corp';
    expect(tracker.changed()).toBe(true);
    const answer = tracker.canLeave();
    expect(isObservable(answer)).toBe(true);
    expect(await firstValueFrom(answer as Observable<boolean>)).toBe(false);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'Project settings' }));
  });

  it('leaves when the user confirms', async () => {
    const { form, tracker } = setup('leave');
    tracker.mark();
    form.name = 'Acme Corp';
    expect(await firstValueFrom(tracker.canLeave() as Observable<boolean>)).toBe(true);
  });

  it('counts a save as the new baseline', () => {
    const { form, tracker } = setup('stay');
    tracker.mark();
    form.name = 'Acme Corp';
    tracker.mark();
    expect(tracker.changed()).toBe(false);
  });

  it('holds the browser tab open only while edits are unsaved', () => {
    const { form, tracker } = setup('stay');
    tracker.mark();
    const clean = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(clean);
    expect(clean.defaultPrevented).toBe(false);
    form.name = 'Acme Corp';
    const dirty = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(dirty);
    expect(dirty.defaultPrevented).toBe(true);
  });
});

describe('dialogFormChangesSinceOpen', () => {
  it('reports a change only after the dialog opened and while the form differs from that moment', () => {
    let name = '';
    const form = dialogFormChangesSinceOpen(() => ({ name }));
    name = 'typed before open';
    expect(form.changed()).toBe(false);
    form.mark();
    expect(form.changed()).toBe(false);
    name = 'Acme';
    expect(form.changed()).toBe(true);
    name = 'typed before open';
    expect(form.changed()).toBe(false);
  });
});
