/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { PermissionPickerComponent } from './permission-picker.component';

@Component({
  standalone: true,
  imports: [PermissionPickerComponent],
  template: `<app-permission-picker [permissions]="permissions" [(selection)]="selection" />`,
})
class Host {
  permissions = [
    { id: 'viewProject', mutating: false },
    { id: 'deleteProject', mutating: true },
  ];
  selection = signal<Record<string, boolean>>({ viewProject: true });
}

describe('PermissionPickerComponent', () => {
  function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    return { fixture, root: fixture.nativeElement as HTMLElement };
  }

  it('splits the permissions into a read and a write group', () => {
    const { root } = render();
    const groups = Array.from(root.querySelectorAll('fieldset')).map((r) => r.textContent?.replace(/\s+/g, ' ').trim());
    expect(groups).toEqual(['Read view project', 'Write delete project']);
  });

  it('hands back a new selection when a permission is ticked', async () => {
    const { fixture, root } = render();
    const before = fixture.componentInstance.selection();
    root.querySelectorAll<HTMLInputElement>('input[type=checkbox]')[1].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.selection()).toEqual({ viewProject: true, deleteProject: true });
    expect(before).toEqual({ viewProject: true });
  });
});
