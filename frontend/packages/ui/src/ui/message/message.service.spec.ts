/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { MessageService } from './message.service';

describe('MessageService', () => {
  let service: MessageService;

  beforeEach(() => {
    vi.useFakeTimers();
    TestBed.configureTestingModule({ providers: [MessageService] });
    service = TestBed.inject(MessageService);
  });

  afterEach(() => vi.useRealTimers());

  it('starts empty', () => {
    expect(service.messages()).toEqual([]);
  });

  it('defaults the severity to info and assigns an id', () => {
    service.add({ summary: 'Saved' });
    const [msg] = service.messages();
    expect(msg.severity).toBe('info');
    expect(typeof msg.id).toBe('number');
  });

  it('keeps ids unique across messages', () => {
    service.add({ summary: 'a', life: 0 });
    service.add({ summary: 'b', life: 0 });
    const ids = service.messages().map((m) => m.id);
    expect(new Set(ids).size).toBe(2);
  });

  it('expires a message after its life', () => {
    service.add({ summary: 'Saved', life: 1000 });
    expect(service.messages().length).toBe(1);
    vi.advanceTimersByTime(1001);
    expect(service.messages().length).toBe(0);
  });

  it('keeps a message with life zero', () => {
    service.add({ summary: 'Sticky', life: 0 });
    vi.advanceTimersByTime(10_000);
    expect(service.messages().length).toBe(1);
  });

  it('removes by id and clears all', () => {
    service.add({ summary: 'a', life: 0 });
    service.add({ summary: 'b', life: 0 });
    service.remove(service.messages()[0].id);
    expect(service.messages().length).toBe(1);
    service.clear();
    expect(service.messages()).toEqual([]);
  });
});
