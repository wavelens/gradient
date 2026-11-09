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
  template: `<app-permission-picker [permissions]="permissions" [viewPermissionIsRequired]="viewRequired()" [(selection)]="selection" />`,
})
class Host {
  permissions = [
    { id: 'viewProject', mutating: false },
    { id: 'editTask', mutating: true },
    { id: 'deleteProject', mutating: true },
  ];
  selection = signal<Record<string, boolean>>({ viewProject: true });
  viewRequired = signal(false);
}

describe('PermissionPickerComponent', () => {
  function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    return { fixture, root: fixture.nativeElement as HTMLElement };
  }

  it('groups the permissions by the lowest built-in role that grants them', () => {
    const { root } = render();
    const groups = Array.from(root.querySelectorAll('fieldset')).map((r) => r.textContent?.replace(/\s+/g, ' ').trim());
    expect(groups).toEqual(['Read View project', 'Write Edit task', 'Admin Delete project']);
  });

  it('hands back a new selection when a permission is ticked', async () => {
    const { fixture, root } = render();
    const before = fixture.componentInstance.selection();
    root.querySelectorAll<HTMLInputElement>('input[type=checkbox]')[2].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.selection()).toEqual({ viewProject: true, editTask: false, deleteProject: true });
    expect(before).toEqual({ viewProject: true });
  });

  it('keeps the view permission ticked and locked when roles require it', async () => {
    const { fixture, root } = render();
    fixture.componentInstance.selection.set({});
    fixture.componentInstance.viewRequired.set(true);
    fixture.detectChanges();
    const boxes = root.querySelectorAll<HTMLInputElement>('input[type=checkbox]');
    expect(boxes[0].checked).toBe(true);
    expect(boxes[0].disabled).toBe(true);
    boxes[1].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.selection()).toEqual({ viewProject: true, editTask: true, deleteProject: false });
  });
});
