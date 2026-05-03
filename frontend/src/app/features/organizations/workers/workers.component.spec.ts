/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { ActivatedRoute, convertToParamMap } from '@angular/router';
import { vi } from 'vitest';
import { WorkersComponent } from './workers.component';
import { WorkersService } from '@core/services/workers.service';
import { OrganizationsService } from '@core/services/organizations.service';

describe('WorkersComponent — no-cache banner', () => {
  let fixture: ComponentFixture<WorkersComponent>;
  let orgsService: { getOrganization: ReturnType<typeof vi.fn>; getSubscribedCaches: ReturnType<typeof vi.fn> };

  beforeEach(async () => {
    const workers = { getWorkers: vi.fn().mockReturnValue(of([])) };
    orgsService = {
      getOrganization: vi.fn().mockReturnValue(of({ id: 'org-uuid', display_name: 'Org' } as any)),
      getSubscribedCaches: vi.fn(),
    };

    await TestBed.configureTestingModule({
      imports: [WorkersComponent],
      providers: [
        provideRouter([]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: WorkersService, useValue: workers },
        { provide: OrganizationsService, useValue: orgsService },
        {
          provide: ActivatedRoute,
          useValue: { snapshot: { paramMap: convertToParamMap({ org: 'demo' }) } },
        },
      ],
    }).compileComponents();
  });

  it('shows the banner when the org has no subscribed caches', () => {
    orgsService.getSubscribedCaches.mockReturnValue(of([]));
    fixture = TestBed.createComponent(WorkersComponent);
    fixture.detectChanges();
    const banner = fixture.nativeElement.querySelector('[data-testid="no-cache-banner"]');
    expect(banner).toBeTruthy();
  });

  it('hides the banner when the org has at least one subscribed cache', () => {
    orgsService.getSubscribedCaches.mockReturnValue(of([{ id: 'c', name: 'cache-1' }]));
    fixture = TestBed.createComponent(WorkersComponent);
    fixture.detectChanges();
    const banner = fixture.nativeElement.querySelector('[data-testid="no-cache-banner"]');
    expect(banner).toBeNull();
  });
});
