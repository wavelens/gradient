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
import { ApiError } from '@core/services/api.service';
import { NotifyService } from '@core/services/notify.service';
import { formatBytes } from '@shared/text';
import { BuildArtefactsComponent } from './build-artefacts.component';

function render(
  getBuildDownloads: () => unknown,
  getBuild: () => unknown = () => of({ derivation_path: '/nix/store/abc-hello-2.12.drv' }),
  downloadBuildArtefactFile: () => unknown = () => of(),
) {
  TestBed.configureTestingModule({
    imports: [BuildArtefactsComponent],
    providers: [
      provideRouter([]),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ project: 'p', buildId: 'b1' }), queryParamMap: convertToParamMap({}) } } },
      { provide: EvaluationsService, useValue: { getBuildDownloads, getBuild, downloadBuildArtefactFile } },
      { provide: AuthService, useValue: { isAuthenticated: () => false } },
      { provide: ProjectsService, useValue: {} },
      { provide: BreadcrumbsService, useValue: { buildPage: vi.fn(() => []) } },
      { provide: NotifyService, useValue: { failure: vi.fn() } },
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

  it('heads the page with the package name instead of the build id', () => {
    const { root } = render(() => of([]));
    expect(root.querySelector('h1')?.textContent?.trim()).toBe('hello-2.12');
  });

  it('names artefact types in words instead of the raw product type', () => {
    const { root } = render(() => of([
      { name: 'hello.tar.gz', path: 'hello.tar.gz', file_type: 'file', subtype: 'binary-dist' },
      { name: 'manual', path: 'manual', file_type: 'doc', subtype: 'manual' },
      { name: 'out', path: 'out', file_type: 'nix-build', subtype: 'out' },
    ]));
    expect([...root.querySelectorAll('gr-row gr-badge')].map((badge) => badge.textContent?.trim())).toEqual(['Binary', 'Manual', 'Nix build']);
  });

  it('puts the build evaluation in the breadcrumb when opened without an origin, with no extra Back button', () => {
    const { root } = render(() => of([]), () => of({ evaluation: 'e1', derivation_path: '/nix/store/abc-hello-2.12.drv' }));
    expect(TestBed.inject(BreadcrumbsService).buildPage).toHaveBeenLastCalledWith('p', { evaluation: 'e1' }, 'b1', 'Artefacts');
    expect(root.querySelector('[slot=actions]')).toBeNull();
  });

  it('hides the size of an artefact when the server does not know it', () => {
    const { root } = render(() => of([
      { name: 'a.tar', path: 'a.tar', file_type: 'file', subtype: 'binary-dist', size: null },
      { name: 'b.tar', path: 'b.tar', file_type: 'file', subtype: 'binary-dist', size: 2048 },
    ]));
    expect([...root.querySelectorAll('[slot=meta]')].map((meta) => meta.textContent?.trim())).toEqual([formatBytes(2048)]);
  });

  it('downloads a file inside the app and reports a failed download in a toast', () => {
    const downloadBuildArtefactFile = vi.fn().mockReturnValue(throwError(() => new ApiError('artefact missing', 404)));
    const { fixture, root } = render(() => of([{ name: 'a b.tar', path: 'a b.tar', file_type: 'file', subtype: 'binary-dist' }]), undefined, downloadBuildArtefactFile);
    const { failure } = TestBed.inject(NotifyService);

    root.querySelector<HTMLButtonElement>('button[title="Download a b.tar"]')!.click();
    fixture.detectChanges();

    expect(downloadBuildArtefactFile).toHaveBeenCalledWith('b1', 'a b.tar');
    expect(failure).toHaveBeenCalledWith("Couldn't download a b.tar", expect.any(ApiError));
    expect(root.querySelector('a[href*="/download/"]')).toBeNull();
  });

  it('opens HTML artefacts in a new tab', () => {
    const { root } = render(() => of([{ name: 'report.html', path: 'report.html', file_type: 'doc', subtype: 'html' }]));
    const link = root.querySelector<HTMLAnchorElement>('a[title="Open report.html"]')!;
    expect(link.getAttribute('href')).toContain('/builds/b1/download/report.html');
    expect(link.target).toBe('_blank');
  });
});
