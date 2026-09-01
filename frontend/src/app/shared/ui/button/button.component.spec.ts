/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ButtonComponent, ButtonSeverity } from './button.component';

@Component({
  standalone: true,
  imports: [ButtonComponent],
  template: `
    <button
      grButton
      [label]="label()"
      [icon]="icon()"
      [iconPos]="iconPos()"
      [severity]="severity()"
      [loading]="loading()"
      [disabled]="disabled()"
      [size]="size()"
    >{{ projected() }}</button>
  `,
})
class HostComponent {
  label = signal('Save');
  icon = signal('');
  iconPos = signal<'left' | 'right'>('left');
  severity = signal<ButtonSeverity>('primary');
  loading = signal(false);
  disabled = signal(false);
  size = signal<'small' | 'normal'>('normal');
  projected = signal('');
}

function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  return { fixture, btn: () => fixture.nativeElement.querySelector('button') as HTMLButtonElement };
}

describe('ButtonComponent', () => {
  it('renders the label', () => {
    const { btn } = render();
    expect(btn().textContent).toContain('Save');
  });

  it('carries the severity class and defaults to primary', () => {
    const { fixture, btn } = render();
    expect(btn().classList).toContain('gr-button--primary');
    fixture.componentInstance.severity.set('danger');
    fixture.detectChanges();
    expect(btn().classList).toContain('gr-button--danger');
    expect(btn().classList).not.toContain('gr-button--primary');
  });

  it('leaves the native disabled property to the caller', () => {
    const { fixture, btn } = render();
    expect(btn().disabled).toBe(false);
    fixture.componentInstance.disabled.set(true);
    fixture.detectChanges();
    expect(btn().disabled).toBe(true);
  });

  it('renders an icon as a material symbol before the label', () => {
    const { fixture, btn } = render();
    fixture.componentInstance.icon.set('add');
    fixture.detectChanges();
    const icon = btn().querySelector('.gr-button__icon')!;
    expect(icon.textContent).toBe('add');
    expect(icon.classList).toContain('material-symbols-outlined');
    expect(icon.compareDocumentPosition(btn().querySelector('.gr-button__label')!))
      .toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });

  it('puts the icon after the label when iconPos is right', () => {
    const { fixture, btn } = render();
    fixture.componentInstance.icon.set('arrow_forward');
    fixture.componentInstance.iconPos.set('right');
    fixture.detectChanges();
    expect(btn().querySelector('.gr-button__icon')!.compareDocumentPosition(btn().querySelector('.gr-button__label')!))
      .toBe(Node.DOCUMENT_POSITION_PRECEDING);
  });

  it('swaps the icon for a spinner and marks itself busy while loading', () => {
    const { fixture, btn } = render();
    fixture.componentInstance.icon.set('add');
    fixture.componentInstance.loading.set(true);
    fixture.detectChanges();
    expect(btn().querySelector('.gr-button__spinner')).toBeTruthy();
    expect(btn().querySelector('.gr-button__icon')).toBeNull();
    expect(btn().getAttribute('aria-busy')).toBe('true');
  });

  it('marks itself icon-only when there is an icon but no label', () => {
    const { fixture, btn } = render();
    fixture.componentInstance.icon.set('delete');
    fixture.componentInstance.label.set('');
    fixture.detectChanges();
    expect(btn().classList).toContain('gr-button--icon-only');
  });

  it('projects content for callers that do not use label', () => {
    const { fixture, btn } = render();
    fixture.componentInstance.label.set('');
    fixture.componentInstance.projected.set('Projected');
    fixture.detectChanges();
    expect(btn().textContent).toContain('Projected');
  });

  it('applies the small size class only when asked', () => {
    const { fixture, btn } = render();
    expect(btn().classList).not.toContain('gr-button--small');
    fixture.componentInstance.size.set('small');
    fixture.detectChanges();
    expect(btn().classList).toContain('gr-button--small');
  });
});
