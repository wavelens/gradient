/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { MessageService } from '../message/message.service';
import { ToastComponent } from './toast.component';

function render() {
  TestBed.configureTestingModule({ imports: [ToastComponent], providers: [MessageService] });
  const fixture = TestBed.createComponent(ToastComponent);
  fixture.detectChanges();
  const service = TestBed.inject(MessageService);
  const items = () => Array.from(fixture.nativeElement.querySelectorAll('.gr-toast__item')) as HTMLElement[];
  return { fixture, service, items };
}

describe('MessageService and ToastComponent', () => {
  it('renders nothing until a message arrives', () => {
    const { items } = render();
    expect(items()).toHaveLength(0);
  });

  it('renders the summary and detail of a message', () => {
    const { fixture, service, items } = render();
    service.add({ severity: 'success', summary: 'Saved', detail: 'Worker updated.', life: 0 });
    fixture.detectChanges();
    expect(items()).toHaveLength(1);
    expect(items()[0].textContent).toContain('Saved');
    expect(items()[0].textContent).toContain('Worker updated.');
  });

  it('classes the item by severity and defaults to info', () => {
    const { fixture, service, items } = render();
    service.add({ severity: 'error', summary: 'Boom', life: 0 });
    service.add({ summary: 'Plain', life: 0 });
    fixture.detectChanges();
    expect(items()[0].classList).toContain('gr-toast__item--error');
    expect(items()[1].classList).toContain('gr-toast__item--info');
  });

  it('dismisses a message when its close button is used', () => {
    const { fixture, service, items } = render();
    service.add({ summary: 'Saved', life: 0 });
    fixture.detectChanges();
    (items()[0].querySelector('.gr-toast__close') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(items()).toHaveLength(0);
  });

  it('expires a message after its life elapses', async () => {
    const { fixture, service, items } = render();
    service.add({ summary: 'Transient', life: 10 });
    fixture.detectChanges();
    expect(items()).toHaveLength(1);
    await new Promise((r) => setTimeout(r, 30));
    fixture.detectChanges();
    expect(items()).toHaveLength(0);
  });

  it('keeps a message with life 0 until dismissed', async () => {
    const { fixture, service, items } = render();
    service.add({ summary: 'Sticky', life: 0 });
    fixture.detectChanges();
    await new Promise((r) => setTimeout(r, 30));
    fixture.detectChanges();
    expect(items()).toHaveLength(1);
  });
});
