/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Injectable, signal } from '@angular/core';
import { Message } from '../types';

export interface ToastMessage extends Message {
  id: number;
}

const DEFAULT_LIFE = 3000;

@Injectable()
export class MessageService {
  private next = 0;
  private queue = signal<ToastMessage[]>([]);
  readonly messages = this.queue.asReadonly();

  add(message: Message): void {
    const entry: ToastMessage = { severity: 'info', ...message, id: this.next++ };
    this.queue.update((all) => [...all, entry]);
    const life = message.life ?? DEFAULT_LIFE;
    if (life > 0) setTimeout(() => this.remove(entry.id), life);
  }

  remove(id: number): void {
    this.queue.update((all) => all.filter((m) => m.id !== id));
  }

  clear(): void {
    this.queue.set([]);
  }
}
