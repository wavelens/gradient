/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ConfirmationService } from './confirmation.service';

describe('ConfirmationService', () => {
  let service: ConfirmationService;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [ConfirmationService] });
    service = TestBed.inject(ConfirmationService);
  });

  it('has nothing pending initially', () => {
    expect(service.pending()).toBeNull();
  });

  it('exposes the pending confirmation', () => {
    service.confirm({ message: 'Delete it?' });
    expect(service.pending()?.message).toBe('Delete it?');
  });

  it('runs accept and clears the pending confirmation', () => {
    let accepted = false;
    service.confirm({ accept: () => (accepted = true) });
    service.accept();
    expect(accepted).toBe(true);
    expect(service.pending()).toBeNull();
  });

  it('runs reject and clears the pending confirmation', () => {
    let rejected = false;
    service.confirm({ reject: () => (rejected = true) });
    service.reject();
    expect(rejected).toBe(true);
    expect(service.pending()).toBeNull();
  });

  it('clears before invoking, so a callback can queue another confirmation', () => {
    service.confirm({ accept: () => service.confirm({ message: 'second' }) });
    service.accept();
    expect(service.pending()?.message).toBe('second');
  });

  it('tolerates a confirmation without callbacks', () => {
    service.confirm({ message: 'no handlers' });
    expect(() => service.accept()).not.toThrow();
    expect(service.pending()).toBeNull();
  });
});
