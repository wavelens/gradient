/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { InteractivityChecker } from '@angular/cdk/a11y';
import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { DialogComponent } from './dialog.component';
import { SelectComponent } from '../select/select.component';

@Component({
  standalone: true,
  imports: [DialogComponent],
  template: `
    <button class="opener">Open</button>
    <gr-dialog
      [(visible)]="open"
      [header]="header()"
      [closable]="closable()"
      [error]="error()"
      [busy]="busy()"
      [confirmDiscard]="discard()"
      width="500px"
      (hide)="hidden.set(hidden() + 1)"
    >
      <p class="body">Are you sure?</p>
      @if (withField()) {
        <input class="field" />
      }
      <div grDialogFooter>
        <button class="gr-button--secondary cancel" (click)="clicks.push('cancel')">Cancel</button>
        <button class="confirm" [disabled]="confirmDisabled()" (click)="clicks.push('confirm')">Delete</button>
      </div>
    </gr-dialog>
  `,
})
class HostComponent {
  open = signal(false);
  header = signal('Delete project');
  closable = signal(true);
  error = signal<string | null>(null);
  busy = signal(false);
  discard = signal(false);
  hidden = signal(0);
  confirmDisabled = signal(false);
  withField = signal(false);
  clicks: string[] = [];
}

class LayoutFreeInteractivityChecker extends InteractivityChecker {
  override isVisible(): boolean {
    return true;
  }
}

const ctrlEnter = () =>
  panel()!.querySelector('.body')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true }));

const escape = () =>
  panel()!.querySelector('.body')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));

function render() {
  TestBed.configureTestingModule({
    imports: [HostComponent],
    providers: [{ provide: InteractivityChecker, useClass: LayoutFreeInteractivityChecker }],
  });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  return fixture;
}

const panel = () => document.querySelector('.gr-dialog');

describe('DialogComponent', () => {
  it('stays out of the DOM until it is made visible', () => {
    const fixture = render();
    expect(panel()).toBeNull();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    expect(panel()).toBeTruthy();
  });

  it('renders the header, the body and the footer slot', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    expect(panel()!.querySelector('.gr-dialog__title')!.textContent).toContain('Delete project');
    expect(panel()!.querySelector('.body')!.textContent).toContain('Are you sure?');
    expect(panel()!.querySelector('.gr-dialog__footer .confirm')).toBeTruthy();
  });

  it('re-projects its content when reopened', () => {
    const fixture = render();
    for (const _ of [0, 1]) {
      fixture.componentInstance.open.set(true);
      fixture.detectChanges();
      expect(panel()!.querySelector('.body')!.textContent).toContain('Are you sure?');
      expect(panel()!.querySelector('.gr-dialog__footer .confirm')).toBeTruthy();
      fixture.componentInstance.open.set(false);
      fixture.detectChanges();
    }
  });

  it('closes and reports back through the two-way binding', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    (panel()!.querySelector('.gr-dialog__close') as HTMLButtonElement).click();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(false);
    expect(panel()).toBeNull();
  });

  it('emits hide when it closes', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    fixture.componentInstance.open.set(false);
    fixture.detectChanges();
    expect(fixture.componentInstance.hidden()).toBe(1);
  });

  it('stays silent when its page is destroyed during the exit animation', () => {
    vi.useFakeTimers();
    onTestFinished(() => {
      vi.useRealTimers();
      vi.restoreAllMocks();
    });
    const realStyle = window.getComputedStyle;
    vi.spyOn(window, 'getComputedStyle').mockImplementation(
      (el) => ({ ...realStyle(el), animationDuration: '0.2s' }) as CSSStyleDeclaration,
    );
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    fixture.componentInstance.open.set(false);
    fixture.detectChanges();
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const error = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const host = fixture.componentInstance;
    fixture.destroy();
    vi.advanceTimersByTime(500);
    expect(host.hidden()).toBe(0);
    expect([...warn.mock.calls, ...error.mock.calls]).toEqual([]);
  });

  it('asks before Escape, backdrop or close button drop unsaved edits', () => {
    const fixture = render();
    const prompt = () => panel()?.querySelector('.gr-dialog__discard');
    const promptButton = (label: string) =>
      Array.from(prompt()?.querySelectorAll<HTMLButtonElement>('button') ?? []).find((b) => b.textContent?.trim() === label)!;
    fixture.componentInstance.discard.set(true);
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();

    escape();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(true);
    expect(prompt()?.textContent).toContain('Discard changes?');
    expect(panel()!.querySelector<HTMLElement>('.gr-dialog__footer')!.hidden).toBe(true);

    escape();
    fixture.detectChanges();
    expect(prompt()).toBeNull();

    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    promptButton('Continue editing').click();
    fixture.detectChanges();
    expect(prompt()).toBeNull();
    expect(fixture.componentInstance.open()).toBe(true);

    (panel()!.querySelector('.gr-dialog__close') as HTMLButtonElement).click();
    fixture.detectChanges();
    promptButton('Discard').click();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(false);
    expect(fixture.componentInstance.hidden()).toBe(1);
  });

  it('hides the close button when it is not closable', () => {
    const fixture = render();
    fixture.componentInstance.closable.set(false);
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    expect(panel()!.querySelector('.gr-dialog__close')).toBeNull();
  });

  it('submits through the primary footer action on Ctrl+Enter', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    ctrlEnter();
    expect(fixture.componentInstance.clicks).toEqual(['confirm']);
  });

  it('ignores Ctrl+Enter while the primary action is disabled', () => {
    const fixture = render();
    fixture.componentInstance.confirmDisabled.set(true);
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    ctrlEnter();
    expect(fixture.componentInstance.clicks).toEqual([]);
  });

  it('applies the requested width and marks itself a modal dialog', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    expect((panel() as HTMLElement).style.width).toBe('500px');
    expect(panel()!.getAttribute('role')).toBe('dialog');
    expect(panel()!.getAttribute('aria-modal')).toBe('true');
  });

  it('shows a failed action inside the open dialog, beside the actions and outside the scrolling content', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.componentInstance.error.set('Name already taken');
    fixture.detectChanges();
    const banner = panel()!.querySelector('gr-message-banner');
    expect(banner?.textContent).toContain('Name already taken');
    expect(banner?.closest('.gr-dialog__content')).toBeNull();
    expect(banner?.parentElement?.nextElementSibling?.classList).toContain('gr-dialog__footer');
    fixture.componentInstance.error.set(null);
    fixture.detectChanges();
    expect(panel()!.querySelector('gr-message-banner')).toBeNull();
  });

  it('closes on a backdrop click like on Escape', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(false);
  });

  it('stays open while busy: Escape, backdrop and the close button do nothing', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.componentInstance.busy.set(true);
    fixture.detectChanges();
    escape();
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    const close = panel()!.querySelector('.gr-dialog__close') as HTMLButtonElement;
    expect(close.disabled).toBe(true);
    close.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(true);
    expect(panel()!.getAttribute('aria-busy')).toBe('true');
  });

  it('does not submit twice through Ctrl+Enter while busy', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.componentInstance.busy.set(true);
    fixture.detectChanges();
    ctrlEnter();
    expect(fixture.componentInstance.clicks).toEqual([]);
  });

  it('keeps a secret that is shown once on screen until the user confirms', () => {
    const fixture = render();
    fixture.componentInstance.closable.set(false);
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    escape();
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement).click();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(true);
  });

  it('emits hide once even when closed twice in a row', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    escape();
    (document.querySelector('.gr-dialog-backdrop') as HTMLElement | null)?.click();
    fixture.detectChanges();
    expect(fixture.componentInstance.hidden()).toBe(1);
  });

  it('focuses the safe footer action of a dialog without fields, not the close button', async () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    await fixture.whenStable();
    expect(document.activeElement?.classList).toContain('cancel');
  });

  it('focuses the first field of a form dialog', async () => {
    const fixture = render();
    fixture.componentInstance.withField.set(true);
    fixture.componentInstance.open.set(true);
    await fixture.whenStable();
    expect(document.activeElement?.classList).toContain('field');
  });

  it('returns focus to the element that opened it', async () => {
    const fixture = render();
    const opener = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.opener')!;
    opener.focus();
    fixture.componentInstance.open.set(true);
    await fixture.whenStable();
    escape();
    fixture.detectChanges();
    expect(document.activeElement).toBe(opener);
  });

  it('moves focus to the main content when the opener left the page', async () => {
    const main = document.createElement('main');
    main.id = 'main-content';
    main.tabIndex = -1;
    document.body.append(main);
    const fixture = render();
    const opener = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.opener')!;
    opener.focus();
    fixture.componentInstance.open.set(true);
    await fixture.whenStable();
    opener.remove();
    escape();
    fixture.detectChanges();
    expect(document.activeElement).toBe(main);
    main.remove();
  });

  it('locks page scrolling while open without moving the page', async () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    await fixture.whenStable();
    expect(document.documentElement.style.overflow).toBe('hidden');
    expect(document.documentElement.style.position).toBe('');
    escape();
    fixture.detectChanges();
    expect(document.documentElement.style.overflow).toBe('');
  });

  it('closes an open dropdown on the first Escape and the dialog on the second', async () => {
    @Component({
      standalone: true,
      imports: [DialogComponent, SelectComponent],
      template: `<gr-dialog [(visible)]="open" header="Pick"><gr-select [options]="['a', 'b']" /></gr-dialog>`,
    })
    class HostWithSelect {
      open = signal(true);
    }
    TestBed.configureTestingModule({ providers: [{ provide: InteractivityChecker, useClass: LayoutFreeInteractivityChecker }] });
    const fixture = TestBed.createComponent(HostWithSelect);
    await fixture.whenStable();
    const trigger = document.querySelector<HTMLButtonElement>('.gr-dialog .gr-select__trigger')!;
    const pressEscape = () => trigger.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    trigger.click();
    fixture.detectChanges();
    expect(document.querySelectorAll('.gr-select__option')).toHaveLength(2);
    pressEscape();
    fixture.detectChanges();
    expect(document.querySelectorAll('.gr-select__option')).toHaveLength(0);
    expect(fixture.componentInstance.open()).toBe(true);
    pressEscape();
    fixture.detectChanges();
    expect(fixture.componentInstance.open()).toBe(false);
  });
});
