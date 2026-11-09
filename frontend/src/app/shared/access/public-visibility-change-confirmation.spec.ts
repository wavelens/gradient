/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { EMPTY, of } from 'rxjs';
import { ConfirmService } from '@shared/confirm';
import { injectSaveSettingsWithPrivateToPublicConfirmation } from './public-visibility-change-confirmation';

function setup() {
  const confirm = vi.fn(() => EMPTY);
  TestBed.configureTestingModule({ providers: [{ provide: ConfirmService, useValue: { confirm } }] });
  const save = TestBed.runInInjectionContext(() => injectSaveSettingsWithPrivateToPublicConfirmation());
  return { save, confirm };
}

describe('public visibility change confirmation', () => {
  it('saves at once unless the save makes the target public', () => {
    const { save, confirm } = setup();
    const request = vi.fn(() => of('ok'));
    const values: string[] = [];
    save({ kind: 'project', target: 'Acme', becomesPublic: false, save: request }).subscribe((value) => values.push(value));
    expect(confirm).not.toHaveBeenCalled();
    expect(values).toEqual(['ok']);
  });

  it('asks before making the target public and leaves the save to the dialog', () => {
    const { save, confirm } = setup();
    const request = vi.fn(() => of('ok'));
    save({ kind: 'cache', target: 'Demo', becomesPublic: true, save: request }).subscribe();
    expect(confirm).toHaveBeenCalledWith(
      expect.objectContaining({ title: 'Make cache public', target: 'Demo', severity: 'primary', action: request }),
    );
    expect(request).not.toHaveBeenCalled();
  });
});
