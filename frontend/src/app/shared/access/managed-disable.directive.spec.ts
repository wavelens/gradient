/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed, ComponentFixture } from '@angular/core/testing';
import { ManagedDisableDirective } from './managed-disable.directive';
import { AccessState } from '@core/models/access.model';

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [ManagedDisableDirective],
  template: `<input [appManagedDisable]="access()" />`,
})
class HostComponent {
  access = signal<AccessState>({ managed: false, canEdit: true, canTrigger: true });
}

describe('ManagedDisableDirective ([appManagedDisable])', () => {
  let fixture: ComponentFixture<HostComponent>;

  function input(): HTMLInputElement {
    return fixture.nativeElement.querySelector('input') as HTMLInputElement;
  }

  beforeEach(() => {
    TestBed.configureTestingModule({ imports: [HostComponent] });
    fixture = TestBed.createComponent(HostComponent);
  });

  it('does not disable when canEdit && !managed', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: true, canTrigger: true });
    fixture.detectChanges();
    expect(input().disabled).toBe(false);
  });

  it('disables when managed=true (regardless of canEdit)', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: true, canTrigger: true });
    fixture.detectChanges();
    expect(input().disabled).toBe(true);
  });

  it('disables when canEdit=false', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: false, canTrigger: false });
    fixture.detectChanges();
    expect(input().disabled).toBe(true);
  });

  it('disables when both managed=true and canEdit=false', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: false, canTrigger: false });
    fixture.detectChanges();
    expect(input().disabled).toBe(true);
  });

  it('sets a tooltip mentioning "managed" when managed', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: true, canTrigger: true });
    fixture.detectChanges();
    expect(input().title.toLowerCase()).toContain('managed');
  });

  it('sets a tooltip mentioning "read-only" when access is read-only', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: false, canTrigger: false });
    fixture.detectChanges();
    expect(input().title.toLowerCase()).toContain('read-only');
  });

  it('removes disabled and tooltip when access becomes writable again', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: true, canTrigger: true });
    fixture.detectChanges();
    expect(input().disabled).toBe(true);

    fixture.componentInstance.access.set({ managed: false, canEdit: true, canTrigger: true });
    fixture.detectChanges();
    expect(input().disabled).toBe(false);
    expect(input().title).toBe('');
  });
});
