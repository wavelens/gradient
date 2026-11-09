/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { AuthService } from '@core/services/auth.service';
import { EvaluationsService } from '@core/services/evaluations.service';
import { ProjectsService } from '@core/services/projects.service';
import { BreadcrumbsService } from '@core/services/breadcrumbs.service';
import { BuildArtefactsComponent } from './build-artefacts.component';

function render(getBuildDownloads: () => unknown, getBuild: () => unknown = () => of({ derivation_path: '/nix/store/abc-hello-2.12.drv' })) {
  TestBed.configureTestingModule({
    imports: [BuildArtefactsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ project: 'p', buildId: 'b1' }), queryParamMap: convertToParamMap({}) } } },
      { provide: EvaluationsService, useValue: { getBuildDownloads, getBuild } },
      { provide: AuthService, useValue: { isAuthenticated: () => false } },
      { provide: ProjectsService, useValue: {} },
      { provide: BreadcrumbsService, useValue: { buildPage: () => [] } },
    ],
  });
  const fixture = TestBed.createComponent(BuildArtefactsComponent);
  fixture.detectChanges();
  return { fixture, root: fixture.nativeElement as HTMLElement };
}

describe('BuildArtefactsComponent', () => {
  it('shows a failed load instead of claiming the build has no artefacts, and loads again on Try Again', () => {
    const getBuildDownloads = vi.fn()
      .mockReturnValueOnce(throwError(() => new Error('store unavailable')))
      .mockReturnValue(of([{ name: 'report.html', path: 'report.html', file_type: 'doc', subtype: 'html' }]));
    const { fixture, root } = render(getBuildDownloads);
    expect(root.textContent).toContain('Failed to load artefacts');
    expect(root.textContent).toContain('store unavailable');
    expect(root.textContent).not.toContain('No artefacts available');

    root.querySelector<HTMLButtonElement>('.gr-load-state__error button')?.click();
    fixture.detectChanges();
    expect(root.textContent).toContain('report.html');
  });

  it('explains how artefacts are produced when the build has none', () => {
    const { root } = render(() => of([]));
    expect(root.textContent).toContain('No artefacts available for this build.');
    expect(root.textContent).toContain('hydra-build-products');
  });

  it('names the package instead of the build id', () => {
    const { root } = render(() => of([]));
    expect(root.textContent).toContain('hello-2.12');
  });

  it('leads back to the log of the build evaluation when opened without an origin', () => {
    const { root } = render(() => of([]), () => of({ evaluation: 'e1', derivation_path: '/nix/store/abc-hello-2.12.drv' }));
    expect(root.querySelector('[slot=actions]')?.getAttribute('href')).toBe('/project/p/log/e1?build=b1');
  });
});
