/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { AccessService } from './access.service';
import { AccessState } from '@core/models/access.model';

const s = (managed: boolean, canEdit: boolean, canTrigger: boolean = canEdit): AccessState => ({
  managed,
  canEdit,
  canTrigger,
});

describe('AccessService', () => {
  let svc: AccessService;

  beforeEach(() => {
    TestBed.configureTestingModule({});
    svc = TestBed.inject(AccessService);
  });

  describe('isWritable', () => {
    it('is true only when canEdit && !managed', () => {
      expect(svc.isWritable(s(false, true))).toBe(true);
      expect(svc.isWritable(s(true, true))).toBe(false);
      expect(svc.isWritable(s(false, false))).toBe(false);
      expect(svc.isWritable(s(true, false))).toBe(false);
    });
  });

  describe('shouldShowWriteAction', () => {
    it('returns canEdit (managed users still see write actions, just disabled)', () => {
      expect(svc.shouldShowWriteAction(s(false, true))).toBe(true);
      expect(svc.shouldShowWriteAction(s(true, true))).toBe(true);
      expect(svc.shouldShowWriteAction(s(false, false))).toBe(false);
      expect(svc.shouldShowWriteAction(s(true, false))).toBe(false);
    });
  });

  describe('shouldDisableInput', () => {
    it('disables when managed OR !canEdit', () => {
      expect(svc.shouldDisableInput(s(false, true))).toBe(false);
      expect(svc.shouldDisableInput(s(true, true))).toBe(true);
      expect(svc.shouldDisableInput(s(false, false))).toBe(true);
      expect(svc.shouldDisableInput(s(true, false))).toBe(true);
    });
  });

  describe('triggerAccess', () => {
    it('mirrors canTrigger into canEdit and forces managed=false', () => {
      const out = svc.triggerAccess(s(true, false, true));
      expect(out).toEqual({ managed: false, canEdit: true, canTrigger: true });
    });

    it('disables when canTrigger is false even if canEdit is true', () => {
      const out = svc.triggerAccess(s(false, true, false));
      expect(out).toEqual({ managed: false, canEdit: false, canTrigger: false });
    });

    it('keeps trigger actions open on a managed project when canTrigger is true', () => {
      // canEdit can be true on managed projects (caller has EditProject) — the
      // service must still strip the managed flag so [appManagedDisable] does
      // not disable trigger buttons.
      const out = svc.triggerAccess(s(true, true, true));
      expect(out.managed).toBe(false);
      expect(out.canEdit).toBe(true);
    });
  });
});
