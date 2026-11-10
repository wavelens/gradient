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

  it('Announce error toasts as alerts', () => {
    const { fixture, service, items } = render();
    service.add({ severity: 'error', summary: 'Boom', life: 0 });
    service.add({ severity: 'success', summary: 'Saved', life: 0 });
    fixture.detectChanges();
    expect(items().map((item) => item.getAttribute('role'))).toEqual(['alert', null]);
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

  describe('placement', () => {
    const added: HTMLElement[] = [];
    function fakeHeader(className: string, bottom: number) {
      const element = document.createElement('header');
      element.className = className;
      element.getBoundingClientRect = () => ({ bottom }) as DOMRect;
      document.body.appendChild(element);
      added.push(element);
    }
    const stack = (fixture: ReturnType<typeof render>['fixture']) => fixture.nativeElement.querySelector('.gr-toast') as HTMLElement;

    afterEach(() => added.splice(0).forEach((element) => element.remove()));

    it('places the stack just below a visible page header', () => {
      fakeHeader('main-header', 56);
      fakeHeader('page-layout__header', 140);
      const { fixture, service } = render();
      service.add({ summary: 'Saved', life: 0 });
      fixture.detectChanges();
      expect(stack(fixture).style.top).toBe('148px');
    });

    it('keeps the default place when the page header is scrolled under the app header', () => {
      fakeHeader('main-header', 56);
      fakeHeader('page-layout__header', 20);
      const { fixture, service } = render();
      service.add({ summary: 'Saved', life: 0 });
      fixture.detectChanges();
      expect(stack(fixture).style.top).toBe('');
    });

    it('keeps the default place on pages without a page header', () => {
      const { fixture, service } = render();
      service.add({ summary: 'Saved', life: 0 });
      fixture.detectChanges();
      expect(stack(fixture).style.top).toBe('');
    });
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
