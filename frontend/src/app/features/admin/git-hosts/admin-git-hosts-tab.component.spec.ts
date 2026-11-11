/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { AdminService } from '@core/services/admin.service';
import { GitHostConnectionsApi } from '@core/services/git-host-connections.service';
import { AdminGitHostsTabComponent } from './admin-git-hosts-tab.component';

function setup() {
  const list = vi.fn().mockReturnValue(of([]));
  TestBed.configureTestingModule({
    imports: [AdminGitHostsTabComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: AdminService, useValue: { requestGithubAppManifest: vi.fn(), fetchGithubAppCredentials: vi.fn() } },
      { provide: GitHostConnectionsApi, useValue: { list } },
      { provide: ActivatedRoute, useValue: { snapshot: { queryParamMap: convertToParamMap({}) } } },
    ],
  });
  const fixture = TestBed.createComponent(AdminGitHostsTabComponent);
  fixture.detectChanges();
  return { fixture, root: fixture.nativeElement as HTMLElement, list };
}

describe('AdminGitHostsTabComponent', () => {
  it('shows the GitHub App setup and the connections of the instance', () => {
    const { root, list } = setup();
    expect(root.querySelector('app-admin-github-app')).not.toBeNull();
    expect(root.textContent).toContain('Instance Connections');
    expect(root.querySelector('app-git-host-connection-list')).not.toBeNull();
    expect(list).toHaveBeenCalledWith({ type: 'instance' });
  });

  it('allows leaving the tab while the GitHub App setup has no unsaved changes', () => {
    const { fixture } = setup();
    expect(fixture.componentInstance.canDeactivate()).toBe(true);
  });
});
