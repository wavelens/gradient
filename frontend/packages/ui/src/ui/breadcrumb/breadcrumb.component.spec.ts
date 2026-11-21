/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { BreadcrumbComponent, type Crumb } from './breadcrumb.component';

function renderBreadcrumbWithCrumbs(trail: Crumb[]): HTMLElement {
  TestBed.configureTestingModule({ providers: [provideRouter([])] });
  const fixture = TestBed.createComponent(BreadcrumbComponent);
  fixture.componentRef.setInput('trail', trail);
  fixture.detectChanges();
  return fixture.nativeElement as HTMLElement;
}

describe('gr-breadcrumb', () => {
  it('link every crumb except the last one, and render the last crumb as the current page', () => {
    const root = renderBreadcrumbWithCrumbs([
      { label: 'acme', link: ['/project', 'acme'] },
      { label: 'Tasks', link: ['/project', 'acme', 'tasks'] },
      { label: 'web', link: ['/project', 'acme', 'tasks', 'web'] },
    ]);

    const links = Array.from(root.querySelectorAll('a.breadcrumb-link')).map((link) => link.getAttribute('href'));
    expect(links).toEqual(['/project/acme', '/project/acme/tasks']);
    const current = root.querySelector('.breadcrumb-current');
    expect(current?.textContent?.trim()).toBe('web');
    expect(current?.getAttribute('aria-current')).toBe('page');
    expect(root.querySelectorAll('.breadcrumb-separator')).toHaveLength(2);
  });

  it('show a placeholder during loading of the crumb name', () => {
    const root = renderBreadcrumbWithCrumbs([{ label: null, link: ['/project', 'acme'] }, { label: 'Tasks' }]);

    expect(root.querySelector('a.breadcrumb-link gr-skeleton')).not.toBeNull();
    expect(root.querySelector('.breadcrumb-current gr-skeleton')).toBeNull();
  });
});
