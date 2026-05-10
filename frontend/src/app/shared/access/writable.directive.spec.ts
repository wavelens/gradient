/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed, ComponentFixture } from '@angular/core/testing';
import { WritableDirective } from './writable.directive';
import { AccessState } from '@core/models/access.model';

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [WritableDirective],
  template: `<button *appWritable="access()">save</button>`,
})
class HostComponent {
  access = signal<AccessState>({ managed: false, canEdit: true });
}

describe('WritableDirective (*appWritable)', () => {
  let fixture: ComponentFixture<HostComponent>;

  beforeEach(() => {
    TestBed.configureTestingModule({ imports: [HostComponent] });
    fixture = TestBed.createComponent(HostComponent);
  });

  it('renders content when canEdit is true', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: true });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('button')).not.toBeNull();
  });

  it('renders content when canEdit and managed are both true', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: true });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('button')).not.toBeNull();
  });

  it('hides content when canEdit is false', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: false });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('button')).toBeNull();
  });

  it('hides content when canEdit is false even if managed is true', () => {
    fixture.componentInstance.access.set({ managed: true, canEdit: false });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('button')).toBeNull();
  });

  it('toggles content when the input changes', () => {
    fixture.componentInstance.access.set({ managed: false, canEdit: true });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('button')).not.toBeNull();

    fixture.componentInstance.access.set({ managed: false, canEdit: false });
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('button')).toBeNull();
  });
});
