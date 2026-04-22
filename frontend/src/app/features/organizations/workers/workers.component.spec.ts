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
import { WorkersComponent } from './workers.component';
import { WorkersService } from '@core/services/workers.service';
import { OrganizationsService } from '@core/services/organizations.service';

describe('WorkersComponent — no-cache banner', () => {
  let fixture: ComponentFixture<WorkersComponent>;
  let orgsService: jasmine.SpyObj<OrganizationsService>;

  beforeEach(async () => {
    const workers = jasmine.createSpyObj<WorkersService>('WorkersService', ['getWorkers']);
    workers.getWorkers.and.returnValue(of([]));
    orgsService = jasmine.createSpyObj<OrganizationsService>('OrganizationsService', [
      'getOrganization', 'getSubscribedCaches',
    ]);
    orgsService.getOrganization.and.returnValue(of({ id: 'org-uuid', display_name: 'Org' } as any));

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
    orgsService.getSubscribedCaches.and.returnValue(of([]));
    fixture = TestBed.createComponent(WorkersComponent);
    fixture.detectChanges();
    const banner = fixture.nativeElement.querySelector('[data-testid="no-cache-banner"]');
    expect(banner).withContext('banner element').toBeTruthy();
  });

  it('hides the banner when the org has at least one subscribed cache', () => {
    orgsService.getSubscribedCaches.and.returnValue(of([{ id: 'c', name: 'cache-1' }]));
    fixture = TestBed.createComponent(WorkersComponent);
    fixture.detectChanges();
    const banner = fixture.nativeElement.querySelector('[data-testid="no-cache-banner"]');
    expect(banner).withContext('banner element').toBeNull();
  });
});
