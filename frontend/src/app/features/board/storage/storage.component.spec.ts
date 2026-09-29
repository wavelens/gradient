import { ComponentFixture, TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { vi } from 'vitest';
import { BoardStorageComponent } from './storage.component';
import { BoardService, BoardStorage } from '@core/services/board.service';

const EMPTY: BoardStorage = {
  granularity: 'minute',
  op_latency: [],
  op_errors: [],
  lane_fill: [],
  send_stalls: [],
  serve_queue: [],
  serve_failures: [],
};

describe('BoardStorageComponent', () => {
  let fixture: ComponentFixture<BoardStorageComponent>;
  const getStorage = vi.fn(() => of(EMPTY));

  beforeEach(async () => {
    getStorage.mockClear();
    await TestBed.configureTestingModule({
      imports: [BoardStorageComponent],
      providers: [{ provide: BoardService, useValue: { getStorage } }],
    }).compileComponents();
    fixture = TestBed.createComponent(BoardStorageComponent);
    fixture.detectChanges();
  });

  it('loads a 6 hour window and renders four charts', () => {
    expect(getStorage).toHaveBeenCalledWith(6);
    expect(fixture.nativeElement.querySelectorAll('gr-metric-chart').length).toBe(4);
  });

  it('refetches when the window changes', () => {
    const button = [...fixture.nativeElement.querySelectorAll('button.window')].find(
      (b: HTMLButtonElement) => b.textContent?.trim() === '24h'
    ) as HTMLButtonElement;
    button.click();
    fixture.detectChanges();

    expect(getStorage).toHaveBeenLastCalledWith(24);
  });
});
