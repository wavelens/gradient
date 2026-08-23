/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { DialogComponent } from './dialog.component';

@Component({
  standalone: true,
  imports: [DialogComponent],
  template: `
    <gr-dialog
      [(visible)]="open"
      [header]="header()"
      [closable]="closable()"
      width="500px"
      (hide)="hidden.set(hidden() + 1)"
    >
      <p class="body">Are you sure?</p>
      <div grDialogFooter><button class="confirm">Delete</button></div>
    </gr-dialog>
  `,
})
class HostComponent {
  open = signal(false);
  header = signal('Delete project');
  closable = signal(true);
  hidden = signal(0);
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
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

  it('hides the close button when it is not closable', () => {
    const fixture = render();
    fixture.componentInstance.closable.set(false);
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    expect(panel()!.querySelector('.gr-dialog__close')).toBeNull();
  });

  it('applies the requested width and marks itself a modal dialog', () => {
    const fixture = render();
    fixture.componentInstance.open.set(true);
    fixture.detectChanges();
    expect((panel() as HTMLElement).style.width).toBe('500px');
    expect(panel()!.getAttribute('role')).toBe('dialog');
    expect(panel()!.getAttribute('aria-modal')).toBe('true');
  });
});
