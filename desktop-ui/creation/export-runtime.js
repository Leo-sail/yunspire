import { Zip, ZipPassThrough, strToU8 } from 'fflate';
import html2canvas from 'html2canvas';
import { jsPDF } from 'jspdf';

const A4_RATIO = 297 / 210;
const MIN_PAGE_HEIGHT = 640;
const MAX_FILE_STEM_LENGTH = 80;

function positiveInteger(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? Math.max(1, Math.round(number)) : fallback;
}

export function normalizeExportFileStem(value) {
  const normalized = String(value || '')
    .normalize('NFKC')
    .replace(/[\\/:*?"<>|\u0000-\u001f]/gu, '-')
    .replace(/\s+/gu, ' ')
    .replace(/[. ]+$/gu, '')
    .trim()
    .slice(0, MAX_FILE_STEM_LENGTH);
  return normalized || 'yunspire-article';
}

export function planRasterPages(totalHeightValue, pageHeightValue) {
  const totalHeight = positiveInteger(totalHeightValue, 1);
  const pageHeight = positiveInteger(pageHeightValue, totalHeight);
  const pages = [];
  for (let offset = 0, index = 0; offset < totalHeight; offset += pageHeight, index += 1) {
    pages.push({
      index,
      offset,
      height: Math.min(pageHeight, totalHeight - offset),
      totalHeight,
    });
  }
  return pages;
}

export function verticalRangeIntersectsPage(page, topValue, bottomValue, overscanValue = 0) {
  const top = Number(topValue);
  const bottom = Number(bottomValue);
  const overscan = Math.max(0, Number(overscanValue) || 0);
  if (!Number.isFinite(top) || !Number.isFinite(bottom) || !page) return false;
  const pageTop = Number(page.offset || 0) - overscan;
  const pageBottom = Number(page.offset || 0) + Number(page.height || 0) + overscan;
  return Math.max(top, bottom) >= pageTop && Math.min(top, bottom) <= pageBottom;
}

function exportDocument(element) {
  const documentValue = element?.ownerDocument;
  if (!element || !documentValue?.body || typeof element.cloneNode !== 'function') {
    throw new TypeError('创作导出需要一个已经渲染的正文元素');
  }
  return documentValue;
}

async function waitForImages(container) {
  await Promise.all([...container.querySelectorAll('img')].map((image) => {
    // A browser marks both successfully loaded and permanently failed images as
    // complete. Waiting for another event after either outcome would deadlock an
    // export forever.
    if (image.complete) return Promise.resolve();
    return new Promise((resolve) => {
      const finish = () => resolve();
      image.addEventListener('load', finish, { once: true });
      image.addEventListener('error', finish, { once: true });
    });
  }));
}

async function waitForRenderableContent(element) {
  const documentValue = exportDocument(element);
  if (documentValue.fonts?.ready) await documentValue.fonts.ready.catch(() => undefined);
  await waitForImages(element);
  await new Promise((resolve) => documentValue.defaultView?.requestAnimationFrame?.(() => resolve()) || resolve());
}

function measuredSize(element) {
  const rectangle = element.getBoundingClientRect();
  const width = positiveInteger(Math.max(element.scrollWidth || 0, rectangle.width || 0), 1);
  const height = positiveInteger(Math.max(element.scrollHeight || 0, rectangle.height || 0), 1);
  return { width, height };
}

function rasterStage(element, { width, totalHeight, backgroundColor }) {
  const documentValue = exportDocument(element);
  const stage = documentValue.createElement('div');
  stage.dataset.yunspireExportStage = 'true';
  Object.assign(stage.style, {
    position: 'fixed',
    left: '-100000px',
    top: '0',
    width: `${width}px`,
    height: '1px',
    overflow: 'hidden',
    margin: '0',
    padding: '0',
    background: backgroundColor,
    zIndex: '-2147483648',
    pointerEvents: 'none',
  });
  const clone = element.cloneNode(true);
  clone.removeAttribute('id');
  Object.assign(clone.style, {
    width: `${width}px`,
    minWidth: `${width}px`,
    height: `${totalHeight}px`,
    minHeight: `${totalHeight}px`,
    maxHeight: 'none',
    margin: '0',
    transform: 'translateY(0)',
    transformOrigin: 'top left',
    boxShadow: 'none',
  });
  stage.append(clone);
  documentValue.body.append(stage);
  return { stage, clone };
}

function reportProgress(callback, payload) {
  if (typeof callback === 'function') callback(payload);
}

export async function* renderCreationRasterPages(element, options = {}) {
  await waitForRenderableContent(element);
  const { width, height: totalHeight } = measuredSize(element);
  const pageHeight = positiveInteger(options.pageHeight, Math.max(MIN_PAGE_HEIGHT, Math.round(width * A4_RATIO)));
  const pages = planRasterPages(totalHeight, pageHeight);
  const scale = Number.isFinite(Number(options.scale)) && Number(options.scale) > 0 ? Number(options.scale) : 1;
  const backgroundColor = String(options.backgroundColor || '#ffffff');
  const { stage, clone } = rasterStage(element, { width, totalHeight, backgroundColor });
  const renderPage = typeof options.renderPage === 'function' ? options.renderPage : html2canvas;

  try {
    await waitForRenderableContent(clone);
    for (const page of pages) {
      reportProgress(options.onProgress, {
        phase: 'rendering',
        page: page.index + 1,
        pageCount: pages.length,
        percent: Math.round((page.index / Math.max(1, pages.length)) * 80),
      });
      stage.style.height = `${page.height}px`;
      clone.style.transform = 'translateY(0)';
      await new Promise((resolve) => stage.ownerDocument.defaultView?.requestAnimationFrame?.(() => resolve()) || resolve());
      let preparedPage = null;
      let canvas = null;
      try {
        if (typeof options.preparePage === 'function') {
          preparedPage = await options.preparePage({ element, stage, clone, page, pages });
        }
        await waitForImages(clone);
        clone.style.transform = `translateY(-${page.offset}px)`;
        await new Promise((resolve) => stage.ownerDocument.defaultView?.requestAnimationFrame?.(() => resolve()) || resolve());
        canvas = await renderPage(stage, {
          backgroundColor,
          scale,
          useCORS: true,
          allowTaint: false,
          logging: false,
          width,
          height: page.height,
          windowWidth: width,
          windowHeight: page.height,
          scrollX: 0,
          scrollY: 0,
        });
        // Yield one page at a time. The canvas and any page-scoped source
        // images are released as soon as the consumer advances.
        yield { ...page, width, pageCount: pages.length, canvas };
      } finally {
        if (canvas) {
          canvas.width = 1;
          canvas.height = 1;
        }
        if (typeof options.releasePage === 'function') {
          await options.releasePage({ element, stage, clone, page, pages, preparedPage });
        }
      }
    }
  } finally {
    stage.remove();
  }
}

function canvasBlob(canvas, mimeType, quality) {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob) resolve(blob);
      else reject(new Error(`无法编码 ${mimeType} 图片页`));
    }, mimeType, quality);
  });
}

async function sha256Blob(blob) {
  if (!globalThis.crypto?.subtle) return '';
  const digest = await globalThis.crypto.subtle.digest('SHA-256', await blob.arrayBuffer());
  return `sha256:${[...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('')}`;
}

function downloadBlob(blob, fileName) {
  const url = URL.createObjectURL(blob);
  try {
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = fileName;
    anchor.rel = 'noopener';
    anchor.click();
  } finally {
    globalThis.setTimeout(() => URL.revokeObjectURL(url), 30_000);
  }
}

function streamingZipArchive() {
  const parts = [];
  let settled = false;
  let resolveArchive;
  let rejectArchive;
  const completed = new Promise((resolve, reject) => {
    resolveArchive = resolve;
    rejectArchive = reject;
  });
  // An interrupted page render can abandon the archive before finish() is
  // awaited. Attach a passive handler so cancellation never surfaces as an
  // unrelated unhandled rejection; finish() still receives the real failure.
  void completed.catch(() => undefined);
  const archive = new Zip((error, chunk, final) => {
    if (settled) return;
    if (error) {
      settled = true;
      rejectArchive(error);
      return;
    }
    if (chunk?.length) parts.push(chunk);
    if (final) {
      settled = true;
      resolveArchive(new Blob(parts, { type: 'application/zip' }));
    }
  });
  return {
    add(name, bytes) {
      if (settled) throw new Error('ZIP 导出已经结束');
      const entry = new ZipPassThrough(name);
      archive.add(entry);
      entry.push(bytes, true);
    },
    async finish() {
      if (!settled) archive.end();
      return completed;
    },
    abort() {
      if (settled) return;
      settled = true;
      archive.terminate();
      rejectArchive(new Error('ZIP 导出已取消'));
    },
  };
}

export async function exportCreationRaster(element, options = {}) {
  const format = String(options.format || 'png').toLowerCase();
  if (!['png', 'jpeg', 'jpg'].includes(format)) throw new Error(`不支持的创作图片格式：${format}`);
  const jpeg = format !== 'png';
  const extension = jpeg ? 'jpg' : 'png';
  const mimeType = jpeg ? 'image/jpeg' : 'image/png';
  const stem = normalizeExportFileStem(options.fileStem);
  let blob = null;
  let pageCount = 0;
  let archive = null;
  const files = [];
  try {
    for await (const page of renderCreationRasterPages(element, options)) {
      pageCount = page.pageCount;
      reportProgress(options.onProgress, {
        phase: 'encoding',
        page: page.index + 1,
        pageCount,
        percent: 80 + Math.round(((page.index + 1) / Math.max(1, pageCount)) * 15),
      });
      const pageBlob = await canvasBlob(page.canvas, mimeType, jpeg ? (Number(options.quality) || 0.92) : undefined);
      if (pageCount === 1) {
        blob = pageBlob;
        continue;
      }
      archive ||= streamingZipArchive();
      const name = `${stem}-${String(page.index + 1).padStart(3, '0')}.${extension}`;
      files.push(name);
      archive.add(name, new Uint8Array(await pageBlob.arrayBuffer()));
    }
    if (!pageCount) throw new Error('创作正文没有可导出的页面');
    if (archive) {
      archive.add('manifest.json', strToU8(JSON.stringify({
        schemaVersion: 1,
        format: extension,
        pageCount,
        files,
        generatedAt: new Date().toISOString(),
      }, null, 2)));
      blob = await archive.finish();
    }
  } catch (error) {
    archive?.abort();
    throw error;
  }
  const fileName = pageCount === 1 ? `${stem}.${extension}` : `${stem}-${extension}-pages.zip`;
  if (options.download !== false) downloadBlob(blob, fileName);
  reportProgress(options.onProgress, { phase: 'completed', page: pageCount, pageCount, percent: 100 });
  return {
    format: pageCount === 1 ? (jpeg ? 'jpeg' : 'png') : 'zip',
    sourceFormat: jpeg ? 'jpeg' : 'png',
    fileName,
    pageCount,
    byteLength: blob.size,
    contentHash: await sha256Blob(blob),
    blob,
  };
}

export async function exportCreationPdf(element, options = {}) {
  const stem = normalizeExportFileStem(options.fileStem);
  let pdf = null;
  let pageCount = 0;
  const pageWidthPoints = 595.28;
  const pageHeightPoints = 841.89;

  for await (const page of renderCreationRasterPages(element, options)) {
    pageCount = page.pageCount;
    reportProgress(options.onProgress, {
      phase: 'encoding',
      page: page.index + 1,
      pageCount,
      percent: 80 + Math.round(((page.index + 1) / pageCount) * 15),
    });
    const blob = await canvasBlob(page.canvas, 'image/jpeg', Number(options.quality) || 0.92);
    const bytes = new Uint8Array(await blob.arrayBuffer());
    if (!pdf) {
      pdf = new jsPDF({ unit: 'pt', format: 'a4', orientation: 'portrait', compress: true, putOnlyUsedFonts: true });
    } else {
      pdf.addPage('a4', 'portrait');
    }
    const imageHeight = Math.min(pageHeightPoints, pageWidthPoints * (page.canvas.height / page.canvas.width));
    pdf.addImage(bytes, 'JPEG', 0, 0, pageWidthPoints, imageHeight, undefined, 'FAST');
  }

  if (!pdf || !pageCount) throw new Error('创作正文没有可导出的页面');
  const blob = pdf.output('blob');
  const fileName = `${stem}.pdf`;
  if (options.download !== false) downloadBlob(blob, fileName);
  reportProgress(options.onProgress, { phase: 'completed', page: pageCount, pageCount, percent: 100 });
  return {
    format: 'pdf',
    fileName,
    pageCount,
    byteLength: blob.size,
    contentHash: await sha256Blob(blob),
    blob,
  };
}
