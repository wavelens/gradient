/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { deEmphasizedSmallTextOpacity, recapSlideColorSchemeColors } from './recap-slide-color-scheme-colors';
import { type RecapSlideStatistic, type YearlyInstanceSummarySlide, yearlyRecapCreditLine } from './yearly-recap-slides';

const exportedRecapSlidePngWidthPixels = 1080;
const yearlyRecapSlidePngHeightPixels = 1920;
const contentMarginPixels = 96;

interface TextColumnLeftOffsetAndWidth {
  left: number;
  width: number;
}

function truncateTextToFitPixelWidth(context: CanvasRenderingContext2D, text: string, maxWidth: number): string {
  if (context.measureText(text).width <= maxWidth) return text;
  let shortened = text;
  while (shortened.length > 1 && context.measureText(`${shortened}…`).width > maxWidth) shortened = shortened.slice(0, -1);
  return `${shortened}…`;
}

function wrapTextIntoLinesThatFitPixelWidth(context: CanvasRenderingContext2D, text: string, maxWidth: number, maxLines: number): string[] {
  const lines: string[] = [];
  let line = '';
  for (const word of text.split(' ')) {
    const candidate = line ? `${line} ${word}` : word;
    if (context.measureText(candidate).width <= maxWidth || !line) line = candidate;
    else {
      lines.push(line);
      line = word;
    }
  }
  if (line) lines.push(line);
  const kept = lines.slice(0, maxLines);
  if (lines.length > maxLines) kept[maxLines - 1] = truncateTextToFitPixelWidth(context, `${kept[maxLines - 1]} ${lines[maxLines]}`, maxWidth);
  return kept;
}

async function fetchLogoPath2DAndViewBoxSize(): Promise<{ path: Path2D; x: number; y: number; width: number; height: number }> {
  const svg = new DOMParser().parseFromString(await (await fetch('/images/logo.svg')).text(), 'image/svg+xml');
  const [x = 0, y = 0, width = 1, height = 1] = (svg.documentElement.getAttribute('viewBox') ?? '').split(/[\s,]+/).map(Number);
  return { path: new Path2D(svg.querySelector('path')?.getAttribute('d') ?? ''), x, y, width, height };
}

export async function drawYearlyRecapSlideToPngBlob(slide: YearlyInstanceSummarySlide, year: number): Promise<Blob> {
  const canvas = document.createElement('canvas');
  canvas.width = exportedRecapSlidePngWidthPixels;
  canvas.height = yearlyRecapSlidePngHeightPixels;
  const context = canvas.getContext('2d');
  if (!context) throw new Error('No 2D canvas for the slide image in this browser.');
  const logo = await fetchLogoPath2DAndViewBoxSize();

  const colors = recapSlideColorSchemeColors[slide.colorScheme];
  const fontFamily = getComputedStyle(document.body).fontFamily;
  const defaultFullContentWidthColumn: TextColumnLeftOffsetAndWidth = {
    left: contentMarginPixels,
    width: exportedRecapSlidePngWidthPixels - 2 * contentMarginPixels,
  };
  const setFont = (size: number, weight: number, color: string) => {
    context.font = `${weight} ${size}px ${fontFamily}`;
    context.fillStyle = color;
  };
  const drawLine = (text: string, y: number, size: number, weight: number, color: string, column = defaultFullContentWidthColumn) => {
    setFont(size, weight, color);
    context.fillText(truncateTextToFitPixelWidth(context, text, column.width), column.left, y);
  };
  const drawWrapped = (
    text: string,
    y: number,
    size: number,
    weight: number,
    color: string,
    maxLines: number,
    column = defaultFullContentWidthColumn,
  ): number => {
    setFont(size, weight, color);
    const lines = wrapTextIntoLinesThatFitPixelWidth(context, text, column.width, maxLines);
    lines.forEach((line, index) => context.fillText(line, column.left, y + index * size * 1.08));
    return y + (lines.length - 1) * size * 1.08;
  };
  const drawStatistic = (statistic: RecapSlideStatistic, y: number, valueSize: number): number => {
    const valueBottom = drawWrapped(statistic.value, y, valueSize, 700, colors.text, 2);
    drawLine(statistic.label, valueBottom + valueSize * 0.55, valueSize * 0.38, 600, colors.accent);
    if (statistic.detail) drawLine(statistic.detail, valueBottom + valueSize * 0.98, valueSize * 0.3, 400, colors.text);
    return valueBottom + valueSize * (statistic.detail ? 1.5 : 1.15);
  };
  const findLargestFontSizeThatFitsTextLineInPixelWidth = (text: string, preferredSize: number, minimumSize: number): number => {
    setFont(preferredSize, 700, colors.text);
    return Math.max(minimumSize, Math.min(preferredSize, (preferredSize * defaultFullContentWidthColumn.width) / context.measureText(text).width));
  };

  context.fillStyle = colors.background;
  context.fillRect(0, 0, exportedRecapSlidePngWidthPixels, yearlyRecapSlidePngHeightPixels);
  context.textBaseline = 'alphabetic';

  const logoHeight = yearlyRecapSlidePngHeightPixels * 0.58;
  const logoScale = logoHeight / logo.height;
  context.save();
  context.globalAlpha = 0.08;
  context.fillStyle = colors.text;
  context.translate(exportedRecapSlidePngWidthPixels * 1.16 - logo.width * logoScale, yearlyRecapSlidePngHeightPixels * 1.04 - logoHeight);
  context.scale(logoScale, logoScale);
  context.translate(-logo.x, -logo.y);
  context.fill(logo.path);
  context.restore();

  const bandWidths = [0.74, 0.52, 0.9];
  colors.bands.forEach((color, index) => {
    const width = exportedRecapSlidePngWidthPixels * bandWidths[index];
    const top = 230 + index * 120;
    context.fillStyle = color;
    context.beginPath();
    context.roundRect(exportedRecapSlidePngWidthPixels - width, top, width + 80, 84, 42);
    context.fill();
  });

  drawLine(`Gradient Wrapped ${year}`, 150, 40, 600, colors.text);

  if (slide.colorScheme === 'intro') {
    drawLine(slide.mainStatistic.value, 1260, findLargestFontSizeThatFitsTextLineInPixelWidth(slide.mainStatistic.value, 380, 120), 700, colors.text);
    drawWrapped(slide.headingText, 1400, 56, 500, colors.text, 3);
  } else if (slide.colorScheme === 'summary') {
    const top = drawWrapped(slide.headingText, 680, 84, 700, colors.text, 3) + 200;
    const columnGap = 48;
    const columnWidth = (defaultFullContentWidthColumn.width - columnGap) / 2;
    [slide.mainStatistic, ...slide.secondaryStatistics].forEach((statistic, index) => {
      const column = { left: contentMarginPixels + (index % 2) * (columnWidth + columnGap), width: columnWidth };
      const valueSize = statistic.countUpTarget === null ? 52 : 72;
      const valueBottom = drawWrapped(statistic.value, top + Math.floor(index / 2) * 220, valueSize, 700, colors.text, 2, column);
      drawLine(statistic.label, valueBottom + 46, 28, 600, colors.accent, column);
    });
  } else {
    let y = drawWrapped(slide.headingText, 680, 84, 700, colors.text, 3) + 220;
    const mainStatisticFontSizeAfterFittingToOneLine = findLargestFontSizeThatFitsTextLineInPixelWidth(
      slide.mainStatistic.value,
      slide.mainStatistic.countUpTarget === null ? 120 : 190,
      80,
    );
    y = drawStatistic(slide.mainStatistic, y + mainStatisticFontSizeAfterFittingToOneLine * 0.4, mainStatisticFontSizeAfterFittingToOneLine);
    const secondaryStatisticValueMaximumFontSize = 76;
    setFont(secondaryStatisticValueMaximumFontSize, 700, colors.text);
    const secondaryStatisticsTotalHeightInValueFontSizes = slide.secondaryStatistics.reduce(
      (sum, statistic) =>
        sum + 1.2 + 1.08 * (wrapTextIntoLinesThatFitPixelWidth(context, statistic.value, defaultFullContentWidthColumn.width, 2).length - 1) + (statistic.detail ? 1.5 : 1.15),
      0,
    );
    const secondaryStatisticsBottomLimitY = yearlyRecapSlidePngHeightPixels - (slide.footnoteText ? 230 : 130);
    const secondaryStatisticValueFontSize = Math.min(
      secondaryStatisticValueMaximumFontSize,
      (secondaryStatisticsBottomLimitY - y) / secondaryStatisticsTotalHeightInValueFontSizes,
    );
    for (const statistic of slide.secondaryStatistics) {
      y = drawStatistic(statistic, y + secondaryStatisticValueFontSize * 1.2, secondaryStatisticValueFontSize);
    }
  }
  if (slide.footnoteText) drawWrapped(slide.footnoteText, yearlyRecapSlidePngHeightPixels - 170, 28, 400, colors.text, 2);
  context.globalAlpha = deEmphasizedSmallTextOpacity;
  drawLine(yearlyRecapCreditLine, yearlyRecapSlidePngHeightPixels - 70, 26, 500, colors.text);
  context.globalAlpha = 1;

  return new Promise((resolve, reject) =>
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error('The browser failed to encode the slide image as PNG.'))), 'image/png'),
  );
}
