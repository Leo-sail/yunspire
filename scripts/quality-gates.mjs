import { readFile, readdir, stat } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';

const root = process.cwd();
const failures = [];

async function collect(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const target = path.join(directory, entry.name);
    if (entry.isDirectory()) files.push(...await collect(target));
    else if (entry.isFile()) files.push(target);
  }
  return files;
}

async function readText(...segments) {
  return (await readFile(path.join(root, ...segments), 'utf8')).replace(/\r\n?/gu, '\n');
}

const html = await readText('desktop-ui/index.html');
const appSource = await readText('desktop-ui/app.js');
const styles = await readText('desktop-ui/styles.css');
const nativeLibrary = await readText('src-tauri/src/lib.rs');
const modelProvider = await readText('src-tauri/src/model_provider.rs');
const capturePipeline = await readText('src-tauri/src/capture_pipeline.rs');
const obsidianAdapter = await readText('src-tauri/src/obsidian.rs');
const documentSkill = await readText('skills/document-content-analysis/SKILL.md');
const documentOrigin = JSON.parse(await readText('skills/document-content-analysis/origin.json'));
const documentExtractor = await readText('skills/document-content-analysis/scripts/extract_document.py');
const windowsPdfHelper = await readText('skills/document-content-analysis/scripts/yunspire_pdf_windows.cpp');
const windowsImageHelper = await readText('skills/document-content-analysis/scripts/yunspire_image_windows.cpp');
const windowsNativeBuild = await readText('scripts/build-windows-native.mjs');
const windowsPythonBuild = await readText('scripts/build-windows-python-runtime.mjs');
const windowsTauri = JSON.parse(await readText('src-tauri/tauri.windows.conf.json'));
const externalImageLocalizer = await readText('skills/document-content-analysis/scripts/external_image_localizer.py');
const webSkill = await readText('skills/web-content-analysis/SKILL.md');
const webExtractor = await readText('skills/web-content-analysis/scripts/extract_web.py');
const videoExtractor = await readText('skills/video-content-analysis/scripts/extract_video.py');
const windowsMediaHelper = await readText('skills/video-content-analysis/scripts/yunspire_media_windows.cpp');
const windowsSpeechHelper = await readText('skills/video-content-analysis/scripts/yunspire_speech_windows.cpp');
const windowsMediaBuild = await readText('scripts/build-windows-media-helper.mjs');
const officeParsers = Object.fromEntries(await Promise.all(['ooxml_word.py', 'ooxml_excel.py', 'ooxml_ppt.py'].map(async (fileName) => [
  fileName,
  await readText('skills/document-content-analysis/scripts', fileName),
])));
const tauri = JSON.parse(await readText('src-tauri/tauri.conf.json'));
const csp = tauri.app?.security?.csp || '';
if (!csp.includes("default-src 'self'")) failures.push('CSP must default to self');
if (csp.includes("script-src 'self' 'unsafe-inline'")) failures.push('inline scripts must remain disabled');
if (/\son(?:click|load|error|submit|change)=/iu.test(html)) failures.push('inline DOM event handler found');
if (!/lang="zh-CN"/u.test(html)) failures.push('document language is not zh-CN');
if (!/aria-live="polite"/u.test(html)) failures.push('no polite live region found');
if (!/aria-modal="true"/u.test(html)) failures.push('modal semantics are missing');
if (!/data-locked-switch="true"/u.test(html)) failures.push('destructive confirmation lock is missing');

const routes = new Set([...html.matchAll(/\bdata-route="([^"]+)"/gu)].map((match) => match[1]));
const views = new Set([...html.matchAll(/\bdata-view="([^"]+)"/gu)].map((match) => match[1]));
if (routes.size !== 10 || views.size !== 10) {
  failures.push(`expected 10 navigation routes and views, found routes=${routes.size} views=${views.size}`);
}
for (const route of routes) {
  if (!views.has(route)) failures.push(`navigation route has no page: ${route}`);
}
for (const view of views) {
  if (!routes.has(view)) failures.push(`page has no primary navigation route: ${view}`);
}
for (const requiredInteraction of [
  "document.querySelectorAll('[data-route]')",
  "document.querySelectorAll('[data-tab]')",
  "document.getElementById('task-drawer-trigger')",
  "document.addEventListener('keydown'",
  "toggleAttribute('inert'",
]) {
  if (!appSource.includes(requiredInteraction)) failures.push(`core interaction handler missing: ${requiredInteraction}`);
}
if (!/\.view\s*\{[^}]*overflow-y:\s*auto/isu.test(styles)) failures.push('page-level vertical scrolling is missing');
if (!/\.task-drawer\s*\{[^}]*position:\s*fixed/isu.test(styles)) failures.push('task drawer must remain a fixed overlay');
if (!/\.drawer-section\s*\{[^}]*overflow-y:\s*auto/isu.test(styles)) failures.push('task drawer content scrolling is missing');

const onboardingSteps = [...html.matchAll(/\bdata-onboarding-step="([0-4])"/gu)].map((match) => match[1]);
if (onboardingSteps.join(',') !== '0,1,2,3,4') failures.push(`first-run onboarding must contain exactly five ordered features; found ${onboardingSteps.join(',') || 'none'}`);
if (!appSource.includes('const ONBOARDING_VERSION = 1')) failures.push('versioned first-run onboarding state is missing');
if (!appSource.includes("if (!openOnboarding()) openAssistantSetup()")) failures.push('first launch must open onboarding before assistant preferences');
const avatarOptions = [...html.matchAll(/\bdata-assistant-avatar="([^"]+)"/gu)].map((match) => match[1]);
if (avatarOptions.length < 8) failures.push(`assistant emoji picker must include at least 8 built-in choices; found ${avatarOptions.length}`);
if (!appSource.includes('function assistantDisplayAvatar()')) failures.push('assistant avatar allowlist renderer is missing');
if (appSource.includes("'云'</span><div><div class=\"message-meta\"")) failures.push('assistant message avatar remains hard-coded');
if (/(?:>|["'])LC(?:<|["'])/u.test(appSource)) failures.push('legacy hard-coded user initials remain in the interface');

for (const chunkCommand of ['begin_capture_upload', 'append_capture_upload_chunk', 'finish_capture_upload']) {
  if (!appSource.includes(`invokeNative('${chunkCommand}'`)) failures.push(`frontend chunked file upload command missing: ${chunkCommand}`);
  if (!nativeLibrary.includes(`capture_pipeline::${chunkCommand}`)) failures.push(`native chunked file upload command is not registered: ${chunkCommand}`);
}
if (!appSource.includes('file.slice(offset, offset + CAPTURE_UPLOAD_CHUNK_BYTES)')) failures.push('files are not read incrementally in bounded chunks');
if (!nativeLibrary.includes('capture_pipeline::discard_capture_attachments')) failures.push('staged attachment discard command is not registered');
if (!appSource.includes("invokeNative('discard_capture_attachments'")) failures.push('failed capture paths do not release staged attachments');
if (!capturePipeline.includes('cleanup_expired_capture_staging')) failures.push('startup cleanup for orphaned capture staging is missing');
for (const retention of ['CAPTURE_UPLOAD_STAGING_RETENTION', 'CAPTURE_ATTACHMENT_STAGING_RETENTION', 'CAPTURE_CLAIM_STAGING_RETENTION']) {
  if (!capturePipeline.includes(`${retention}: Duration = Duration::ZERO`)) failures.push(`orphaned capture staging is not removed on the next startup: ${retention}`);
}
if (!capturePipeline.includes('stage_capture_attachment') || !capturePipeline.includes('staged_attachment_id')) failures.push('native attachment staging token flow is incomplete');
if (!appSource.includes("invokeNative('prepare_capture_image_analysis_input'")) failures.push('frontend image analysis derivation command is missing');
if (!nativeLibrary.includes('capture_pipeline::prepare_capture_image_analysis_input')) failures.push('native image analysis derivation command is not registered');
if (!capturePipeline.includes('fn capture_image_analysis_input(') || !capturePipeline.includes('run_sips_derivative')) failures.push('native image analysis derivation flow is incomplete');
if (!capturePipeline.includes('#[cfg(any(target_os = "macos", target_os = "windows"))]\nconst MODEL_ANALYSIS_IMAGE_DERIVATIVE_TIMEOUT')) {
  failures.push('model image derivative timeout must be available on macOS and Windows');
}
for (const imageIntegrityPrimitive of [
  'read_verified_direct_image_bytes',
  '读取模型图片分析输入期间原始图片发生变化',
  'ensure_original_image_unchanged',
  'normalize_expected_capture_sha256',
  'ensure_sips_decode_resource_budget',
  'physical_memory_bytes',
  'available_memory_bytes',
  'available_disk_bytes',
  'validate_image_decode_resource_budget',
]) {
  if (!capturePipeline.includes(imageIntegrityPrimitive)) failures.push(`native image integrity/resource gate is missing: ${imageIntegrityPrimitive}`);
}
if (!obsidianAdapter.includes('claim_staged_capture_attachment') || !obsidianAdapter.includes('hash_file_streaming')) failures.push('Obsidian staged attachment claim or hash verification is missing');
if (/附件总大小超过\s*128\s*MB/iu.test(appSource)) failures.push('legacy 128 MB assistant attachment rejection remains');
if (/\bMAX_(?:VIDEO|MEDIA|HLS_OBJECT)_BYTES\b/u.test(`${capturePipeline}\n${videoExtractor}`)) failures.push('product-level video or HLS byte limit remains');
if (!videoExtractor.includes('def write_hls_object(') || !videoExtractor.includes('output.write(chunk)')) failures.push('HLS media is not streamed directly to disk');
for (const windowsMediaPrimitive of [
  'MFCreateSourceReaderFromURL',
  'MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING',
  'resize_for_model',
  'frame_difference',
  'candidate_interval',
  'frame_timestamps_ms',
  'MFAudioFormat_PCM',
  'speech-audio.wav',
  'windows_media_codec_unavailable',
]) {
  if (!windowsMediaHelper.includes(windowsMediaPrimitive)) failures.push(`Windows native media primitive is missing: ${windowsMediaPrimitive}`);
}
for (const windowsSpeechPrimitive of ['CLSID_SpInprocRecognizer', 'LoadDictation', 'SPBindToFile', 'GetResultTimes', 'ResolveLocaleName', 'SpEnumTokens', 'SpGetLanguageFromToken', 'SetRecognizer', 'windows_sapi_language_unavailable', 'windows_sapi_transcript_unavailable']) {
  if (!windowsSpeechHelper.includes(windowsSpeechPrimitive)) failures.push(`Windows native speech primitive is missing: ${windowsSpeechPrimitive}`);
}
for (const windowsBuildPrimitive of ['VsDevCmd.bat', 'yunspire-media.exe', 'yunspire-speech.exe', 'mfreadwrite.lib', 'sapi.lib']) {
  if (!windowsMediaBuild.includes(windowsBuildPrimitive)) failures.push(`Windows media packaging primitive is missing: ${windowsBuildPrimitive}`);
}
for (const windowsNativeBuildPrimitive of ['/std:c++20', 'msvc-cxx20-mt-v2', 'oleaut32.lib']) {
  if (!windowsNativeBuild.includes(windowsNativeBuildPrimitive)) {
    failures.push(`Windows document/image helper build primitive is missing: ${windowsNativeBuildPrimitive}`);
  }
}
for (const [label, buildScript] of [['document/image', windowsNativeBuild], ['media/speech', windowsMediaBuild]]) {
  if (!buildScript.includes('windowsVerbatimArguments: true')) {
    failures.push(`Windows ${label} helper build must preserve cmd.exe quoting for Visual Studio paths`);
  }
}
for (const [label, helperSource] of [
  ['PDF', windowsPdfHelper],
  ['image', windowsImageHelper],
  ['media', windowsMediaHelper],
  ['speech', windowsSpeechHelper],
]) {
  if (!helperSource.includes('#define NOMINMAX')) failures.push(`Windows ${label} helper must disable min/max macros`);
}
const mediaStreamIndexCasts = windowsMediaHelper.match(/static_cast<DWORD>\(MF_SOURCE_READER_/gu) || [];
if (mediaStreamIndexCasts.length < 12) failures.push(`Windows media stream indexes are not type-safe: ${mediaStreamIndexCasts.length}/12`);
const mediaSampleNullChecks = windowsMediaHelper.match(/sample\.Get\(\) == nullptr/gu) || [];
if (mediaSampleNullChecks.length !== 2) failures.push(`Windows media COM sample checks are incomplete: ${mediaSampleNullChecks.length}/2`);
if (windowsPdfHelper.includes('fs::u8path(')) failures.push('Windows PDF helper still uses deprecated C++20 filesystem::u8path');
const wholePhraseCasts = windowsSpeechHelper.match(/static_cast<ULONG>\(SP_GETWHOLEPHRASE\)/gu) || [];
if (wholePhraseCasts.length !== 2) failures.push(`Windows speech whole-phrase indexes are not type-safe: ${wholePhraseCasts.length}/2`);
if (!videoExtractor.includes('yunspire-media.exe') || !videoExtractor.includes('yunspire-speech.exe')) failures.push('Windows packaged media helpers are not dispatched by the video extractor');
if (!appSource.includes('function resolveHistoricalImageReferences(')) failures.push('historical image reference resolver is missing');
if (!appSource.includes("mode === 'initial' ? `图片记忆")) failures.push('first image analysis memory path is missing');
const attachmentContextBlock = appSource.match(/async function prepareAssistantAttachmentContext\([\s\S]*?\n\}/u)?.[0] || '';
if (/\.dataUrl\s*=|data:image\//u.test(attachmentContextBlock)) failures.push('ordinary assistant attachment context must not resend image data URLs');
if (!attachmentContextBlock.includes('imageAnalysisText(attachment)')) failures.push('ordinary assistant context does not reuse persisted image analysis');

if (/\.slice\(0,\s*4\s*\*\s*1024\s*\*\s*1024\)/u.test(appSource)) failures.push('model analysis still silently truncates content at 4 MB');
for (const requiredBatchingPrimitive of [
  'MODEL_ANALYSIS_REQUEST_MAX_BYTES',
  'modelAnalysisContentBytes',
  'partitionModelAnalysesForConsolidation',
  'MODEL_CONSOLIDATION_TARGET_BYTES',
]) {
  if (!appSource.includes(requiredBatchingPrimitive)) failures.push(`model byte-aware batching primitive missing: ${requiredBatchingPrimitive}`);
}
for (const streamingImagePrimitive of [
  'async function analyzeCaptureContentWithModel(',
  'streamingVisualPreparation: true',
  'await flushLocalBatch()',
  'hashBoundImageCount',
  'image_hash_bindings_complete',
  'capturePreparedImageBinding',
]) {
  if (!appSource.includes(streamingImagePrimitive)) failures.push(`capture streaming visual primitive missing: ${streamingImagePrimitive}`);
}
if (appSource.includes('const extractedImageDataUrls = []')) failures.push('capture still prepares every local image data URL before model batching');
if (!modelProvider.includes('文件整体不受此限制，请由云枢分批处理')) failures.push('native model boundary is still described as a file-size limit');
for (const imageBindingPrimitive of [
  'pub struct CaptureImageBinding',
  'prepare_capture_analysis_images',
  'image_observation_constraints',
  'image_bindings: Option<Vec<CaptureImageBinding>>',
  '视觉输入哈希与 assetId=',
  '返回了未绑定的 reference_id=',
  '"image_bindings": image_bindings',
]) {
  if (!modelProvider.includes(imageBindingPrimitive)) failures.push(`structured model image binding is missing: ${imageBindingPrimitive}`);
}

if (documentSkill.includes('yunspire.cleaned-workbook.v1')) failures.push('document Skill still describes the obsolete workbook v1 contract');
if (!documentSkill.includes('yunspire.cleaned-workbook.v2') || !documentSkill.includes('yunspire.office-document.v2')) {
  failures.push('document Skill does not declare both Office v2 schemas');
}
for (const documentContractPrimitive of [
  '仅对 OOXML 图片关系以及 Markdown',
  '普通超链接',
  '质量门禁必须阻断',
  'Agent 库/资料库/原文/',
  '不建立实体图谱、向量索引或混合检索',
]) {
  if (!documentSkill.includes(documentContractPrimitive)) failures.push(`document Skill contract is missing: ${documentContractPrimitive}`);
}
if (/\b(?:MAX_ROWS|MAX_COLUMNS|ROW_LIMIT|COLUMN_LIMIT)\b/u.test(officeParsers['ooxml_excel.py'])) {
  failures.push('Excel parser contains a legacy row or column truncation constant');
}
const expectedDocumentScripts = [
  'scripts/external_image_localizer.py',
  'scripts/extract_document.py',
  'scripts/ooxml_excel.py',
  'scripts/ooxml_ppt.py',
  'scripts/ooxml_word.py',
  'scripts/yunspire_pdf.m',
  'scripts/yunspire_image_windows.cpp',
  'scripts/yunspire_pdf_windows.cpp',
].sort();
if (JSON.stringify([...(documentOrigin.scripts || [])].sort()) !== JSON.stringify(expectedDocumentScripts)) {
  failures.push('document Skill origin must declare the complete Office v2 implementation set');
}
for (const pdfContractPrimitive of [
  'yunspire.pdf-document.v1',
  'Windows.Data.Pdf',
  '不设置文件大小或页数上限',
  'integrity.status="incomplete"',
]) {
  if (!documentSkill.includes(pdfContractPrimitive)) failures.push(`document Skill Windows PDF contract is missing: ${pdfContractPrimitive}`);
}
for (const pdfExtractorPrimitive of [
  'def _windows_pdf_adapter_path(',
  'def _windows_pdf_result(',
  'yunspire.windows-pdf.v1',
  'pdf_pages_missing:',
  'pdf_render_incomplete',
  '"format": "yunspire.pdf-document.v1"',
  '"model_analysis_input": True',
]) {
  if (!documentExtractor.includes(pdfExtractorPrimitive)) failures.push(`Windows PDF extraction gate is missing: ${pdfExtractorPrimitive}`);
}
if (documentExtractor.includes('platform_pdf_analysis_unavailable')) failures.push('legacy Windows PDF platform rejection remains');
for (const pdfNativePrimitive of [
  'PdfDocument::LoadFromFileAsync',
  'document.GetPage(index)',
  'page.RenderToStreamAsync',
  'BitmapEncoder::JpegEncoderId()',
  'pdf_page_model_image_budget_unavailable',
  'pdf_render_incomplete',
  'pages.size() != page_count',
]) {
  if (!windowsPdfHelper.includes(pdfNativePrimitive)) failures.push(`Windows native PDF helper is missing: ${pdfNativePrimitive}`);
}
for (const imageNativePrimitive of [
  'CLSID_WICImagingFactory',
  'CreateDecoderFromFilename',
  'CreateBitmapScaler',
  'GUID_ContainerFormatJpeg',
  'GlobalMemoryStatusEx',
  'GetDiskFreeSpaceExW',
  'image_output_jpeg_signature_invalid',
  'image_output_dimensions_mismatch',
  'yunspire.windows-image-derivative.v1',
]) {
  if (!windowsImageHelper.includes(imageNativePrimitive)) failures.push(`Windows WIC image helper is missing: ${imageNativePrimitive}`);
}
for (const buildPrimitive of [
  'yunspire_pdf_windows',
  'yunspire_image_windows',
  'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
  'createSmokePdf()',
  "derivativePayload.schema !== 'yunspire.windows-image-derivative.v1'",
  'await smokeTest()',
]) {
  if (!windowsNativeBuild.includes(buildPrimitive)) failures.push(`Windows native helper build/smoke gate is missing: ${buildPrimitive}`);
}
for (const runtimePrimitive of [
  'https://www.python.org/ftp/python/',
  'f6cca216a359be84797cabb54149ce5e062afb16cc7567eb7fc51cacb2d86b65',
  'EXPECTED_SHA256',
  'EXPECTED_MD5',
  'YUNSPIRE_RUNTIME.json',
  'python313._pth',
  'await smokeTest()',
]) {
  if (!windowsPythonBuild.includes(runtimePrimitive)) failures.push(`Windows embedded Python runtime gate is missing: ${runtimePrimitive}`);
}
if (!capturePipeline.includes('yunspire-runtime') || !capturePipeline.includes('python.exe')) {
  failures.push('Windows packaged Python runtime is not resolved by the native capture pipeline');
}
const windowsResources = windowsTauri.bundle?.resources || {};
for (const [source, destination] of [
  ['target/yunspire-runtime/python/', 'runtime/python/'],
  ['target/yunspire-native/yunspire_pdf_windows.exe', 'skills/document-content-analysis/scripts/yunspire_pdf_windows.exe'],
  ['target/yunspire-native/yunspire_image_windows.exe', 'skills/document-content-analysis/scripts/yunspire_image_windows.exe'],
]) {
  if (windowsResources[source] !== destination) failures.push(`Windows native bundle resource mapping is missing: ${source}`);
}
for (const [fileName, source] of Object.entries(officeParsers)) {
  if (!source.includes('attachment://')) failures.push(`${fileName} does not preserve Obsidian attachment placeholders`);
  for (const localizationPrimitive of ['ExternalImageLocalizer', 'external_asset_directory', 'localization_summary', 'external_image_localization', '外链图片本地化失败：']) {
    if (!source.includes(localizationPrimitive)) failures.push(`${fileName} is missing external-image localization primitive: ${localizationPrimitive}`);
  }
  for (const integrityPrimitive of ['"integrity"', '"status"', '"errors"', '"checks"']) {
    if (!source.includes(integrityPrimitive)) failures.push(`${fileName} is missing strict Office integrity output: ${integrityPrimitive}`);
  }
}
if (!officeParsers['ooxml_word.py'].includes('for node in declared_image_nodes:')) {
  failures.push('Word fallback image scan must inspect only declared blip/imagedata nodes');
}
for (const officeGatePrimitive of ['def office_integrity_errors(', 'office_structure_incomplete:', 'errors.extend(office_integrity_errors(structured, path))', '"integrity_status"']) {
  if (!documentExtractor.includes(officeGatePrimitive)) failures.push(`Office partial-parse blocking gate is missing: ${officeGatePrimitive}`);
}
for (const requiredLinkPolicy of ['"auto_open": False', '"auto_fetch": False', '"capture_requires_explicit_user_request"']) {
  if (!documentExtractor.includes(requiredLinkPolicy)) failures.push(`embedded-link safety policy missing: ${requiredLinkPolicy}`);
}
for (const folderImagePrimitive of [
  'namespace_capture_positions',
  'deduplicate_capture_attachments',
  'materialize_attachments',
  'rewrite_attachment_placeholders',
]) {
  if (!documentExtractor.includes(folderImagePrimitive)) failures.push(`folder image position/dedup primitive is missing: ${folderImagePrimitive}`);
}
for (const externalImageContractPrimitive of [
  'external_image_contract',
  '"capture_requires_explicit_user_request": False',
  '"external_image_failures"',
  'external_image_localization_incomplete',
]) {
  if (!documentExtractor.includes(externalImageContractPrimitive)) failures.push(`document external-image result contract is missing: ${externalImageContractPrimitive}`);
}
for (const localizerPrimitive of [
  'FETCH_POLICY = "public-http-image-v1"',
  'class ExternalImageLocalizer',
  'def _resolve_public_addresses(',
  'def localize(',
  'def attachment_payload(',
  'REDIRECT_STATUSES',
  '"private_address"',
  '"https_downgrade"',
  'response.getheader("Content-Type")',
  '_detect_image_format(header)',
  'destination.write(block)',
  'hashlib.sha256()',
  'os.fsync(destination.fileno())',
  'CAPACITY_RESERVE_RATIO',
  'AVAILABLE_RESERVE_RATIO',
  'BATCH_FOOTPRINT_RESERVE_RATIO',
  'def _ensure_disk_headroom(',
  '"required_additional_bytes"',
  '"batch_committed_bytes"',
  '"dynamic_reserve_bytes"',
  '"status": "localized"',
  '"status": "failed"',
  '"redirect_chain"',
  '"local_attachment_path"',
]) {
  if (!externalImageLocalizer.includes(localizerPrimitive)) failures.push(`external-image localizer hardening is missing: ${localizerPrimitive}`);
}
if (/free\s*-\s*32\s*\*\s*1024\s*\*\s*1024/u.test(externalImageLocalizer)
  || /free\s*<\s*32\s*\*\s*1024\s*\*\s*1024/u.test(externalImageLocalizer)) {
  failures.push('external-image localizer still uses a fixed 32 MiB disk reserve');
}
if (/MAX_(?:IMAGE|ASSET|TOTAL|BATCH)(?:_FILE)?_?(?:SIZE|BYTES)/u.test(externalImageLocalizer)) {
  failures.push('external-image localizer contains a fixed single-image or batch byte ceiling');
}
for (const localizationSummaryField of ['external_asset_count', 'localized_asset_count', 'failed_asset_count', 'all_external_images_localized']) {
  if (!externalImageLocalizer.includes(`"${localizationSummaryField}"`)) failures.push(`external-image summary field is missing: ${localizationSummaryField}`);
}
for (const webContractPrimitive of [
  '同一内容流位置',
  '不得设置图片数量上限',
  '稳定 `asset_id`',
  '独立 `reference_id`',
  '已本地化图片只从本地附件字节提交一次',
  '`attachment://<reference_id>`',
]) {
  if (!webSkill.includes(webContractPrimitive)) failures.push(`web Skill contract is missing: ${webContractPrimitive}`);
}
for (const webExtractorPrimitive of [
  'class PinnedHTTPConnection',
  'class PinnedHTTPSConnection',
  'resolve_public_addresses(host, port)',
  'server_hostname=self.host',
  'authorized_headers(current_url)',
  'attachment://{reference_id}',
  'web-image-reference-',
  '"asset_id": f"sha256:{digest}"',
  'asset["references"] = []',
  '"placement_required"] = True',
  '"web_external_image_localization_incomplete"',
  '"localized_image_urls"',
  '"safety_boundaries"',
  '"block_without_partial_write"',
]) {
  if (!webExtractor.includes(webExtractorPrimitive)) failures.push(`web image-preservation primitive is missing: ${webExtractorPrimitive}`);
}
if (/images\s*\[\s*:\s*(?:8|100)\s*\]/u.test(webExtractor)) failures.push('web extractor still truncates images by a fixed item count');
if (/content_markdown[^\n]*\[\s*:\s*2_000_000\s*\]/u.test(webExtractor)) failures.push('web faithful Markdown is still silently truncated');
if (webExtractor.includes('"parse_limits_applied": []')) failures.push('web extractor falsely reports no limits while safety byte boundaries are active');
if (!capturePipeline.includes('"--attachment-output-dir".to_string()')
  || !capturePipeline.includes('stage_result_attachments(&mut result, directory.path())?')) {
  failures.push('web images are not streamed through the native isolated attachment staging path');
}
if (!appSource.includes('result.localized_image_urls') || !appSource.includes('!localized.has(url)')) {
  failures.push('localized web images can still be submitted to the model a second time as remote URLs');
}
if (!appSource.includes("error === 'web_external_image_localization_incomplete'")) {
  failures.push('web external-image failures are not wired into the blocking capture quality gate');
}
if (!appSource.includes('attachment?.placement_required === true')) {
  failures.push('web attachment placement requirements are ignored by the dual-Vault writer');
}
if (officeParsers['ooxml_word.py'].includes("output.append(f\"![{alt}]({item['target']})\")")) {
  failures.push('Word external images still render as auto-fetching Markdown images');
}
for (const wordImageLinkPrimitive of ['hlinkClick', 'hlinkHover', 'image_hyperlink', 'drawing_hyperlink', 'image_reference_id']) {
  if (!officeParsers['ooxml_word.py'].includes(wordImageLinkPrimitive)) failures.push(`Word image-link provenance is missing: ${wordImageLinkPrimitive}`);
}
if (Object.values(officeParsers).some((source) => source.includes('[外部图片未自动读取：'))) {
  failures.push('legacy external-image no-fetch placeholder remains in an Office parser');
}
for (const pptPositionPrimitive of ['source_layer', 'shape_fill_image', 'crop_ooxml', 'covered_coordinates']) {
  if (!officeParsers['ooxml_ppt.py'].includes(pptPositionPrimitive)) failures.push(`PPT position or relationship primitive is missing: ${pptPositionPrimitive}`);
}
for (const excelPrimitive of ['expanded_shared_expression', 'cached_value', 'sheet_order', 'anchor_context', 'external_target']) {
  if (!officeParsers['ooxml_excel.py'].includes(excelPrimitive)) failures.push(`Excel formula, worksheet, or image primitive is missing: ${excelPrimitive}`);
}
if (!obsidianAdapter.includes('fn materialize_capture_raw_markdown(')
  || !obsidianAdapter.includes('原文仍有未解析的本地附件占位')) {
  failures.push('unresolved attachment placeholders are not blocked by the native dual-Vault writer');
}
for (const dualVaultPrimitive of [
  'function resolveCaptureVaultTargets(',
  'function captureStorageStem(',
  "match(/^sha256:([a-f0-9]{64})$/iu)",
  "invokeNative('prepare_capture_vault_writes'",
  'const rawNoteIncluded = prepared.rawNoteIncluded !== false;',
  'const expectedNoteCount = rawNoteIncluded ? 2 : 1;',
  'previews.length !== expectedNoteCount',
  '双库写入计划没有同时生成忠实原文与 Agent 理解稿',
]) {
  if (!appSource.includes(dualVaultPrimitive)) failures.push(`dual-Vault capture frontend contract is missing: ${dualVaultPrimitive}`);
}
const captureStorageStemBlock = appSource.match(/function captureStorageStem\([\s\S]*?\n\}/u)?.[0] || '';
if (!captureStorageStemBlock.includes('match[1].toLowerCase()') || /match\[1\][\s\S]*?\.slice\(/u.test(captureStorageStemBlock)) {
  failures.push('capture storage path must retain the complete normalized content SHA-256');
}
if (!captureStorageStemBlock.includes('return `${match[1].toLowerCase()}/${title}`;')) {
  failures.push('capture storage path must keep the complete hash in a directory so Obsidian Graph shows a readable note basename');
}
for (const openInObsidianPrimitive of [
  "invokeNative('open_vault_note_in_obsidian'",
  'openInObsidian.dataset.vaultId',
  'openInObsidian.dataset.relativePath',
  'obsidian::open_vault_note_in_obsidian',
]) {
  if (!`${appSource}\n${nativeLibrary}`.includes(openInObsidianPrimitive)) failures.push(`native Obsidian note opening contract is missing: ${openInObsidianPrimitive}`);
}
for (const nativeDualVaultPrimitive of [
  'pub struct CaptureVaultWriteInput',
  'raw_vault_id: String',
  'agent_vault_id: String',
  'raw_note_included: bool',
  'let raw_note_included = raw_root != agent_root;',
  'fn capture_image_observations(',
  'fn build_agent_capture_markdown(',
  'let mut agent_body = strip_markdown_frontmatter(&analysis_markdown).to_string();',
  'markdown.push_str("\\n## 分析内容\\n\\n");',
  'knowledge_association: obsidian-tags-and-wikilinks',
  'pub fn prepare_capture_vault_writes(',
  'pub fn commit_capture_batch(',
  '批次必须来自同一次完整模型分析',
  '批次写入失败并已回滚',
]) {
  if (!obsidianAdapter.includes(nativeDualVaultPrimitive)) failures.push(`dual-Vault capture native contract is missing: ${nativeDualVaultPrimitive}`);
}
for (const noOverwritePrimitive of [
  '采集目标已存在，已阻止覆盖忠实原文',
  '采集目标已存在，已阻止覆盖 Agent 理解稿',
  '采集目标已存在，已阻止覆盖原始附件',
]) {
  if (!obsidianAdapter.includes(noOverwritePrimitive)) failures.push(`capture no-overwrite contract is missing: ${noOverwritePrimitive}`);
}
for (const stableReferencePrimitive of [
  'validate_capture_attachment_reference_owners',
  'attachment_position_reference_ids',
  '附件引用 {key} 同时指向多个 asset_id',
  '原文缺少 asset_id={asset_id} 的部分图片位置',
]) {
  if (!obsidianAdapter.includes(stableReferencePrimitive)) failures.push(`stable image reference contract is missing: ${stableReferencePrimitive}`);
}
const captureImageBindingValidationStart = obsidianAdapter.indexOf('fn validate_capture_image_bindings(');
const captureImageBindingValidationEnd = obsidianAdapter.indexOf('\nfn capture_analysis_text(', captureImageBindingValidationStart);
const captureImageBindingValidationBlock = captureImageBindingValidationStart >= 0
  && captureImageBindingValidationEnd > captureImageBindingValidationStart
  ? obsidianAdapter.slice(captureImageBindingValidationStart, captureImageBindingValidationEnd)
  : '';
for (const imageBindingValidationPrimitive of [
  'capture_image_bindings(analysis)?',
  'attachment_position_reference_ids(attachment)?',
  '缺少结构化 image binding',
  '允许位置与 image binding reference_ids 冲突',
  '原始字节数与 image binding 冲突',
  '原件 SHA-256 与 image binding 冲突',
  '暂存图片附件 asset_id={asset_id} 缺少原件 SHA-256',
]) {
  if (!captureImageBindingValidationBlock.includes(imageBindingValidationPrimitive)) {
    failures.push(`Obsidian image-binding validation gate is missing: ${imageBindingValidationPrimitive}`);
  }
}
const captureImageAnalysisBlockStart = obsidianAdapter.indexOf('fn capture_image_analysis_block(');
const captureImageAnalysisBlockEnd = obsidianAdapter.indexOf('\nfn strip_markdown_frontmatter(', captureImageAnalysisBlockStart);
const captureImageAnalysisBlock = captureImageAnalysisBlockStart >= 0
  && captureImageAnalysisBlockEnd > captureImageAnalysisBlockStart
  ? obsidianAdapter.slice(captureImageAnalysisBlockStart, captureImageAnalysisBlockEnd)
  : '';
for (const persistedImageBindingPrimitive of [
  '"asset_id": binding.asset_id',
  '"original_sha256": binding.original_sha256',
  '"analysis_input_sha256": binding.analysis_sha256',
  '"original_byte_length": binding.original_byte_length',
  '"analysis_byte_length": binding.analysis_byte_length',
  '"analysis_mime_type": binding.analysis_mime_type',
  '"derived": binding.derived',
  '"reference_ids": binding.reference_ids',
  '结构化视觉输入绑定',
]) {
  if (!captureImageAnalysisBlock.includes(persistedImageBindingPrimitive)) {
    failures.push(`Agent Markdown image-binding persistence is missing: ${persistedImageBindingPrimitive}`);
  }
}
const prepareAssetWriteStart = obsidianAdapter.indexOf('fn prepare_asset_write_inner(');
const prepareAssetWriteEnd = obsidianAdapter.indexOf('\nfn discard_prepared_capture_writes(', prepareAssetWriteStart);
const prepareAssetWriteBlock = prepareAssetWriteStart >= 0 && prepareAssetWriteEnd > prepareAssetWriteStart
  ? obsidianAdapter.slice(prepareAssetWriteStart, prepareAssetWriteEnd)
  : '';
for (const stagedAttachmentVerificationPrimitive of [
  'claim_staged_capture_attachment(&token, &approval_id)?',
  'fs::metadata(&path)',
  'hash_file_streaming(&path)',
  'normalize_capture_sha256(value, "附件 expected_sha256")',
  'expected_sha256',
  'content_hash',
  'remove_claimed_capture_attachment(&path)',
  '附件哈希与提取结果不一致',
  '暂存附件哈希与提取结果不一致',
]) {
  if (!prepareAssetWriteBlock.includes(stagedAttachmentVerificationPrimitive)) {
    failures.push(`staged attachment byte/hash revalidation is missing: ${stagedAttachmentVerificationPrimitive}`);
  }
}
const prepareCaptureVaultWritesStart = obsidianAdapter.indexOf('fn prepare_capture_vault_writes_inner(');
const prepareCaptureVaultWritesEnd = obsidianAdapter.indexOf('\n#[tauri::command]\npub fn discard_asset_write(', prepareCaptureVaultWritesStart);
const prepareCaptureVaultWritesBlock = prepareCaptureVaultWritesStart >= 0
  && prepareCaptureVaultWritesEnd > prepareCaptureVaultWritesStart
  ? obsidianAdapter.slice(prepareCaptureVaultWritesStart, prepareCaptureVaultWritesEnd)
  : '';
for (const stagedBindingConflictPrimitive of [
  'asset_preview.byte_length != binding.original_byte_length',
  '的实际字节数与 image binding 冲突',
  '采集目标已存在，已阻止覆盖原始附件',
]) {
  if (!prepareCaptureVaultWritesBlock.includes(stagedBindingConflictPrimitive)) {
    failures.push(`staged attachment binding conflict rejection is missing: ${stagedBindingConflictPrimitive}`);
  }
}
for (const modelUnderstandingPrimitive of [
  'analysis_markdown 不是简短摘要',
  'image_observations 每项必须返回 asset_id、reference_id',
  '它不是实体图谱',
  'normalize_image_observations',
  'normalize_document_relations',
]) {
  if (!modelProvider.includes(modelUnderstandingPrimitive)) failures.push(`model-understood source contract is missing: ${modelUnderstandingPrimitive}`);
}
for (const provenanceField of ['"source_kind": "text_span"', '"text_offset_start": offset_start', '"line": line', '"column": column']) {
  if (!documentExtractor.includes(provenanceField)) failures.push(`plain-text link provenance field missing: ${provenanceField}`);
}
if (!documentExtractor.includes('"parse_limits_applied": []') || !documentExtractor.includes('"truncated": False')) {
  failures.push('document extraction does not explicitly report lossless parsing metadata');
}
if (!documentExtractor.includes('os.walk(path, followlinks=False)')) failures.push('folder extraction does not explicitly disable symlink traversal');
if (/\.slice\(0,\s*(?:128|512)\s*\)/u.test(appSource)) failures.push('file-internal links still have a silent 128 or 512 item cap');
if (!appSource.includes('embedded_link_occurrences') || !appSource.includes('partitionDeterministicCaptureRequests')) failures.push('file-internal link occurrence batching is missing');
if (!appSource.includes('let extractedTitle = sourceName;') || !appSource.includes('let warningCount = 0;')) failures.push('capture failure reporting uses block-scoped extraction metadata');
if (!appSource.includes('let runCaptureMemory = null;') || !appSource.includes('discardCaptureStagedAttachments(runCaptureMemory?.result)')) failures.push('capture failure cleanup is not isolated to the current run');

const requiredSlashCommands = ['help', 'new', 'clear', 'rename', 'compact', 'reflect', 'style', 'image', 'edit'];
const slashCommandBlock = appSource.match(/const assistantSlashCommands = \[([\s\S]*?)\n\];/u)?.[1] || '';
const declaredSlashCommands = [...slashCommandBlock.matchAll(/\bname:\s*['"]([^'"]+)['"]/gu)]
  .map((match) => match[1]);
if (declaredSlashCommands.join(',') !== requiredSlashCommands.join(',')) {
  failures.push(`slash command set must be exactly ${requiredSlashCommands.join('/')}; found ${declaredSlashCommands.join('/') || 'none'}`);
}
if (!/<div\b[^>]*\bdata-slash-command-menu\b[^>]*\brole="listbox"[^>]*>/iu.test(html)
  && !/<div\b[^>]*\brole="listbox"[^>]*\bdata-slash-command-menu\b[^>]*>/iu.test(html)) {
  failures.push('slash command menu must expose role="listbox"');
}
if (!/\.slash-command-menu\s*\{[^}]*\bposition:\s*absolute[^}]*\bbottom:\s*calc\(/isu.test(styles)) {
  failures.push('slash command listbox must expand upward from the composer');
}
const slashPrompt = modelProvider.match(/const ASSISTANT_SLASH_COMMAND_PROMPT:\s*&str\s*=\s*"([\s\S]*?)";/u)?.[1] || '';
for (const command of requiredSlashCommands) {
  if (!slashPrompt.includes(`/${command}`)) failures.push(`assistant slash-command prompt is missing /${command}`);
}

const handlerSource = nativeLibrary.match(/tauri::generate_handler!\[([\s\S]*?)\]\s*(?:\)|;)/u)?.[1] || '';
const registeredCommands = new Set(
  [...handlerSource.matchAll(/(?:\b[a-z][a-z0-9_]*::)?([a-z][a-z0-9_]*)\s*,/gu)].map((match) => match[1]),
);
const invokedCommands = new Set(
  [...appSource.matchAll(/invokeNative\(['"]([a-z][a-z0-9_]*)['"]/gu)].map((match) => match[1]),
);
for (const command of invokedCommands) {
  if (!registeredCommands.has(command)) failures.push(`frontend invokes an unregistered native command: ${command}`);
}

const images = [...html.matchAll(/<img\b([^>]*)>/giu)];
for (const image of images) {
  if (!/\balt="[^"]*"/u.test(image[1])) failures.push(`image without alt text: ${image[0].slice(0, 120)}`);
}

const dist = path.join(root, 'dist');
try {
  const assets = await collect(dist);
  let total = 0;
  for (const file of assets) {
    const size = (await stat(file)).size;
    total += size;
    const extension = path.extname(file);
    if (extension === '.js' && size > 2 * 1024 * 1024) failures.push(`JavaScript bundle exceeds 2 MB: ${path.relative(root, file)}`);
    if (extension === '.css' && size > 768 * 1024) failures.push(`CSS bundle exceeds 768 KB: ${path.relative(root, file)}`);
  }
  if (total > 12 * 1024 * 1024) failures.push(`desktop distribution exceeds 12 MB: ${total} bytes`);
} catch {
  failures.push('dist is missing; run the production frontend build first');
}

if (failures.length) {
  console.error('QUALITY_GATES_FAILED');
  failures.forEach((failure) => console.error(`- ${failure}`));
  process.exit(1);
}
console.log(`QUALITY_GATES_OK images=${images.length}`);
