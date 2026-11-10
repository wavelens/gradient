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
  template: `<app-permission-picker [permissions]="permissions" [serverWidePermissions]="serverWide()" [selectablePermissionIds]="selectable()" [viewPermissionIsRequired]="viewRequired()" [(selection)]="selection" />`,
})
class Host {
  permissions = [
    { id: 'viewProject', mutating: false, permission_explanation: 'See the project.' },
    { id: 'editTask', mutating: true, permission_explanation: 'Change a task.' },
    { id: 'deleteProject', mutating: true, permission_explanation: 'Delete the project.' },
  ];
  selection = signal<Record<string, boolean>>({ viewProject: true });
  viewRequired = signal(false);
  serverWide = signal<{ id: string; mutating: boolean; permission_explanation: string }[]>([]);
  selectable = signal<string[] | null>(null);
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
    expect(groups).toEqual(['Read View project See the project.', 'Write Edit task Change a task.', 'Admin Delete project Delete the project.']);
  });

  it('shows the server explanation under each permission label', () => {
    const { root } = render();
    expect(Array.from(root.querySelectorAll('.item-text small')).map((r) => r.textContent)).toEqual(['See the project.', 'Change a task.', 'Delete the project.']);
  });

  it('keeps permissions outside the selectable list visible but disabled and unselected', () => {
    const { fixture, root } = render();
    fixture.componentInstance.selection.set({ viewProject: true, deleteProject: true });
    fixture.componentInstance.selectable.set(['viewProject', 'editTask']);
    fixture.detectChanges();
    const boxes = root.querySelectorAll<HTMLInputElement>('input[type=checkbox]');
    expect(Array.from(boxes).map((box) => box.disabled)).toEqual([false, false, true]);
    expect(boxes[2].checked).toBe(false);
  });

  it('shows server-wide permissions in their own group and includes them in the selection', async () => {
    const { fixture, root } = render();
    fixture.componentInstance.serverWide.set([{ id: 'createProject', mutating: true, permission_explanation: 'Make a project.' }]);
    fixture.detectChanges();
    const groups = Array.from(root.querySelectorAll('fieldset')).map((r) => r.textContent?.replace(/\s+/g, ' ').trim());
    expect(groups.at(-1)).toBe('Whole Server Create project Make a project.');
    root.querySelectorAll<HTMLInputElement>('input[type=checkbox]')[3].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.selection()).toEqual({ viewProject: true, editTask: false, deleteProject: false, createProject: true });
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
