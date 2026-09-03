/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { TaskLayoutComponent } from './task-layout.component';

describe('TaskLayoutComponent', () => {
  it('is a column shell so the routed page inherits the height app-root hands it', () => {
    TestBed.configureTestingModule({
      imports: [TaskLayoutComponent],
      providers: [provideRouter([])],
    });
    const fixture = TestBed.createComponent(TaskLayoutComponent);
    fixture.detectChanges();

    const host = getComputedStyle(fixture.nativeElement as HTMLElement);
    expect(host.display).toBe('flex');
    expect(host.flexDirection).toBe('column');
  });
});
