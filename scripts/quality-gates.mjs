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

function escapeRegularExpression(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/gu, '\\$&');
}

function importedLocalName(source, moduleSpecifier, importedName) {
  const imports = source.matchAll(/\bimport\s*\{([\s\S]*?)\}\s*from\s*(['"])([^'"]+)\2\s*;?/gu);
  for (const match of imports) {
    if (match[3] !== moduleSpecifier) continue;
    const bindings = match[1]
      .replace(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/gu, ' ')
      .split(',')
      .map((binding) => binding.trim())
      .filter(Boolean);
    for (const binding of bindings) {
      const parsed = binding.match(/^([A-Za-z_$][\w$]*)(?:\s+as\s+([A-Za-z_$][\w$]*))?$/u);
      if (parsed?.[1] === importedName) return parsed[2] || parsed[1];
    }
  }
  return null;
}

function callsIdentifier(source, identifier, { awaited = false } = {}) {
  if (!identifier) return false;
  const prefix = awaited ? '\\bawait\\s+' : '(?:^|[^\\w$])';
  return new RegExp(`${prefix}${escapeRegularExpression(identifier)}\\s*\\(`, 'u').test(source);
}

function invokesNativeCommand(source, command) {
  return new RegExp(`\\binvokeNative\\s*\\(\\s*['"]${escapeRegularExpression(command)}['"]`, 'u').test(source);
}

function registersRustCommand(source, moduleName, command) {
  return new RegExp(`\\b${escapeRegularExpression(moduleName)}\\s*::\\s*${escapeRegularExpression(command)}\\b`, 'u').test(source);
}

function extractJavaScriptFunction(source, functionName) {
  const declaration = new RegExp(
    `\\b(?:async\\s+)?function\\s+${escapeRegularExpression(functionName)}\\s*\\([\\s\\S]*?\\)\\s*\\{`,
    'u',
  ).exec(source);
  if (!declaration) return '';
  const openingBrace = declaration.index + declaration[0].lastIndexOf('{');
  let depth = 0;
  let state = 'code';
  for (let index = openingBrace; index < source.length; index += 1) {
    const character = source[index];
    const next = source[index + 1];
    if (state === 'lineComment') {
      if (character === '\n') state = 'code';
      continue;
    }
    if (state === 'blockComment') {
      if (character === '*' && next === '/') {
        state = 'code';
        index += 1;
      }
      continue;
    }
    if (state !== 'code') {
      if (character === '\\') {
        index += 1;
        continue;
      }
      if ((state === 'singleQuote' && character === "'")
        || (state === 'doubleQuote' && character === '"')
        || (state === 'template' && character === '`')) state = 'code';
      continue;
    }
    if (character === '/' && next === '/') {
      state = 'lineComment';
      index += 1;
    } else if (character === '/' && next === '*') {
      state = 'blockComment';
      index += 1;
    } else if (character === "'") state = 'singleQuote';
    else if (character === '"') state = 'doubleQuote';
    else if (character === '`') state = 'template';
    else if (character === '{') depth += 1;
    else if (character === '}') {
      depth -= 1;
      if (depth === 0) return source.slice(declaration.index, index + 1);
    }
  }
  return '';
}

const html = await readText('desktop-ui/index.html');
const appSource = await readText('desktop-ui/app.js');
const styles = await readText('desktop-ui/styles.css');
const nativeLibrary = await readText('src-tauri/src/lib.rs');
const modelProvider = await readText('src-tauri/src/model_provider.rs');
const assistantSkillRoutingPrompt = await readText('prompts/runtime/assistant-skill-routing.txt');
const captureAnalysisSystemPrompt = await readText('prompts/runtime/capture-analysis-system.txt');
const assistantSlashCommandPrompt = await readText('prompts/runtime/assistant-slash-command-system.txt');
const nativeCreation = await readText('src-tauri/src/creation/mod.rs');
const writingPanelSource = await readText('desktop-ui/creation/writing-panel.js');
const executionControllerSource = await readText('desktop-ui/creation/execution-controller.js');
const durableAssetsSource = await readText('desktop-ui/creation/durable-assets.js');
const skillLifecycle = await readText('src-tauri/src/skill_lifecycle.rs');
const skillExecutionRuntime = await readText('desktop-ui/skill-execution-runtime.js');
const embeddedLinkRuntime = await readText('desktop-ui/embedded-link-runtime.js');
const runtimeDatabase = await readText('src-tauri/src/runtime_db.rs');
const capturePipeline = await readText('src-tauri/src/capture_pipeline.rs');
const obsidianAdapter = await readText('src-tauri/src/obsidian.rs');
const vaultBatch = await readText('src-tauri/src/vault_batch.rs');
const documentSkill = await readText('skills/document-content-analysis/SKILL.md');
const documentOrigin = JSON.parse(await readText('skills/document-content-analysis/origin.json'));
const documentExtractor = await readText('skills/document-content-analysis/scripts/extract_document.py');
const windowsPdfHelper = await readText('skills/document-content-analysis/scripts/yunspire_pdf_windows.cpp');
const windowsImageHelper = await readText('skills/document-content-analysis/scripts/yunspire_image_windows.cpp');
const windowsNativeBuild = await readText('scripts/build-windows-native.mjs');
const windowsPythonBuild = await readText('scripts/build-windows-python-runtime.mjs');
const licenseGenerator = await readText('scripts/generate-third-party-notices.mjs');
const packageConfig = JSON.parse(await readText('package.json'));
const windowsTauri = JSON.parse(await readText('src-tauri/tauri.windows.conf.json'));
const externalImageLocalizer = await readText('skills/document-content-analysis/scripts/external_image_localizer.py');
const webSkill = await readText('skills/web-content-analysis/SKILL.md');
const webExtractor = await readText('skills/web-content-analysis/scripts/extract_web.py');
const mediaDiscovery = await readText('skills/video-content-analysis/scripts/media_discovery.py');
const videoExtractor = await readText('skills/video-content-analysis/scripts/extract_video.py');
const windowsMediaHelper = await readText('skills/video-content-analysis/scripts/yunspire_media_windows.cpp');
const windowsSpeechHelper = await readText('skills/video-content-analysis/scripts/yunspire_speech_windows.cpp');
const windowsMediaBuild = await readText('scripts/build-windows-media-helper.mjs');
const windowsReleaseVerifier = await readText('scripts/verify-windows-release.mjs');
const officeParsers = Object.fromEntries(await Promise.all(['ooxml_word.py', 'ooxml_excel.py', 'ooxml_ppt.py'].map(async (fileName) => [
  fileName,
  await readText('skills/document-content-analysis/scripts', fileName),
])));
const tauri = JSON.parse(await readText('src-tauri/tauri.conf.json'));
const thirdPartyNotices = await readText(
  'src-tauri',
  'target',
  'yunspire-licenses',
  'THIRD_PARTY_NOTICES.txt',
).catch(() => '');
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
const expectedPrimaryRoutes = new Set(['dashboard', 'search', 'create', 'reports']);
const allowedAuxiliaryViews = new Set(['agent', 'capture', 'audit', 'settings']);
if (routes.size !== expectedPrimaryRoutes.size || [...routes].some((route) => !expectedPrimaryRoutes.has(route))) {
  failures.push(`expected primary navigation routes ${[...expectedPrimaryRoutes].join(',')}; found ${[...routes].join(',') || 'none'}`);
}
for (const route of routes) {
  if (!views.has(route)) failures.push(`navigation route has no page: ${route}`);
}
for (const view of views) {
  if (!routes.has(view) && !allowedAuxiliaryViews.has(view)) failures.push(`unregistered auxiliary page: ${view}`);
}
for (const removedViewName of ['skills', 'tasks']) {
  if (views.has(removedViewName)) failures.push(`removed ${removedViewName} page must not return`);
}
if (/data-view="(?:skills|tasks)"/u.test(styles)) failures.push('removed Skills or Tasks page selectors must not return');
if (!/class="r10-topbar"/u.test(html)) failures.push('current top navigation is missing');
if (/class="[^"]*\bsidebar\b/u.test(html)) failures.push('removed sidebar markup must not return');
if (!/\.app-shell\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\)[^}]*grid-template-rows:\s*var\(--size-top-navigation\)\s+minmax\(0,\s*1fr\)/su.test(styles)) {
  failures.push('top-navigation application grid is missing');
}
if (/\.app-shell\.sidebar-collapsed\b/u.test(styles)) failures.push('removed sidebar-collapse layout must not return');
for (const assistantShellPrimitive of [
  'class="agent-layout execution-collapsed"',
  'data-execution-toggle',
  'function setExecutionCollapsed(',
  'const selected = [...selectedCapabilityIds]',
]) {
  if (!html.includes(assistantShellPrimitive) && !appSource.includes(assistantShellPrimitive)) failures.push(`AI single-workspace primitive is missing: ${assistantShellPrimitive}`);
}
for (const composerPrimitive of [
  'data-attachment-actions-trigger',
  'data-attachment-actions-menu',
  'data-tool-menu-trigger',
  'toolCapability',
  'composer.addEventListener(\'drop\'',
  'droppedComposerFiles',
]) {
  if (!html.includes(composerPrimitive) && !appSource.includes(composerPrimitive)) failures.push(`composer attachment/tool primitive is missing: ${composerPrimitive}`);
}
if (!/\.filter-pane\s*\{[^}]*display:\s*none/isu.test(styles) && !/<aside class="filter-pane"[^>]*hidden/iu.test(html)) {
  failures.push('knowledge-base filters must remain hidden from the user');
}
for (const assistantOnlySkillPrimitive of [
  'data-skill-create-with-assistant',
  'data-skill-manage-with-assistant',
]) {
  if (!html.includes(assistantOnlySkillPrimitive)) failures.push(`AI-only Skill entry is missing: ${assistantOnlySkillPrimitive}`);
}
for (const skillExecutionPrimitive of [
  'async function executeSelectedUserSkills(',
  "invokeNative('list_routable_skills')",
  'skillExecutionRuntime.execute(',
  'expectedPayloadHash',
  'execution.skill.payloadHash',
  'userSkillSnapshots',
]) {
  if (!appSource.includes(skillExecutionPrimitive)) failures.push(`controlled user Skill execution is missing: ${skillExecutionPrimitive}`);
}
if (!skillExecutionRuntime.includes("await invoke('execute_skill', { input: request })")) {
  failures.push('controlled user Skill runtime does not invoke the native execute_skill command');
}
if (!nativeLibrary.includes('skill_lifecycle::execute_skill')) failures.push('native Skill execution command is not registered');
for (const nativeSkillExecutionPrimitive of [
  'pub async fn execute_skill(',
  'execution_snapshot_in_connection(',
  'validate_declared_schema(',
  'skill.execution.started',
  'skill.execution.succeeded',
  'expected_payload_hash',
]) {
  if (!skillLifecycle.includes(nativeSkillExecutionPrimitive)) failures.push(`native controlled Skill execution is missing: ${nativeSkillExecutionPrimitive}`);
}
for (const skillModelPrimitive of [
  'APPROVED_SKILL_EXECUTION_SYSTEM_PROMPT',
  'execute_approved_skill_model(',
  'yunspire.approved-skill-execution.v1',
]) {
  if (!modelProvider.includes(skillModelPrimitive)) failures.push(`approved Skill model boundary is missing: ${skillModelPrimitive}`);
}
for (const thirdPartySkillUiPrimitive of [
  "new Set(['list', 'run', 'create', 'install'",
  'function handoffSkillInstallationToAssistant()',
  "invokeNative('install_skill_from_github'",
  'userConfirmed: true',
  'skill.evaluationPassed === true',
  '自动批准、默认启用并进入可路由集合',
]) {
  if (!appSource.includes(thirdPartySkillUiPrimitive)) failures.push(`AI-confirmed third-party Skill install UI is missing: ${thirdPartySkillUiPrimitive}`);
}
if (!html.includes('data-skill-install-with-assistant')) failures.push('AI third-party Skill install entry is missing from the tool menu');
if (!nativeLibrary.includes('skill_lifecycle::install_skill_from_github')) failures.push('native third-party Skill install command is not registered');
for (const thirdPartySkillNativePrimitive of [
  'pub async fn install_skill_from_github(',
  'normalize_github_skill_url(',
  '.redirect(Policy::none())',
  'response.status().is_redirection()',
  'response.bytes_stream()',
  'MAX_REMOTE_SKILL_BYTES',
  'String::from_utf8(bytes)',
  'url.query().is_some() || url.fragment().is_some()',
  'skill.source.imported',
  '"sourceUrl"',
  '"sourceHash"',
  'capabilities: Vec::new()',
  'user_confirmed',
  'finalize_confirmed_import(',
]) {
  if (!skillLifecycle.includes(thirdPartySkillNativePrimitive)) failures.push(`restricted GitHub Skill importer is missing: ${thirdPartySkillNativePrimitive}`);
}
const importedSkillFinalization = skillLifecycle.match(/fn finalize_confirmed_import\([\s\S]*?\n\}/u)?.[0] || '';
const confirmedImportSequence = [
  'evaluate_candidate(',
  'if !evaluation.passed {',
  'return Ok(evaluation.skill);',
  'decide_candidate(',
  'approved: true',
  'change_activation(',
  'SkillActivationAction::Enable',
];
for (const confirmedImportPrimitive of confirmedImportSequence) {
  if (!importedSkillFinalization.includes(confirmedImportPrimitive)) {
    failures.push(`confirmed third-party Skill import finalization is missing: ${confirmedImportPrimitive}`);
  }
}
const confirmedImportPositions = confirmedImportSequence.map((primitive) => importedSkillFinalization.indexOf(primitive));
if (confirmedImportPositions.some((position) => position < 0)
  || confirmedImportPositions.some((position, index) => index > 0 && position <= confirmedImportPositions[index - 1])) {
  failures.push('confirmed third-party Skill import must evaluate, short-circuit failures, approve, then enable in order');
}
for (const thirdPartySkillModelPrimitive of ['skill_action=install', 'source_url', '第三方 Skill 只能导入 name、description 和 instructions']) {
  if (!assistantSkillRoutingPrompt.includes(thirdPartySkillModelPrimitive)) failures.push(`third-party Skill model contract is missing: ${thirdPartySkillModelPrimitive}`);
}
for (const embeddingUiPrimitive of [
  "const modelRoles = ['chat', 'analysis', 'image', 'embedding'];",
  "embedding: '向量'",
  "if (provider === 'anthropic') assignments.embedding = [];",
  "role === 'embedding' && profile?.provider === 'anthropic'",
  "role === 'embedding' && profile.provider === 'anthropic'",
  "invokeNative('get_neural_embedding_index_status'",
  "invokeNative('rebuild_neural_embedding_index'",
  "refresh.disabled = !isTauriRuntime || state === 'loading'",
  'neuralEmbeddingRebuildPollTimer = window.setInterval',
  'progress.style.transform = `scaleX(${percent / 100})`',
]) {
  if (!appSource.includes(embeddingUiPrimitive)) failures.push(`neural Embedding UI integration is missing: ${embeddingUiPrimitive}`);
}
for (const embeddingHtmlPrimitive of [
  'data-embedding-index-status',
  'role="progressbar"',
  'data-refresh-embedding-index',
  'data-rebuild-embedding-index',
  '神经语义索引',
]) {
  if (!html.includes(embeddingHtmlPrimitive)) failures.push(`neural Embedding settings surface is missing: ${embeddingHtmlPrimitive}`);
}
const restoreModelConfigurations = appSource.match(/async function restoreModelConfigurations\(\)[\s\S]*?\n\}/u)?.[0] || '';
if (!restoreModelConfigurations.includes('await loadNeuralEmbeddingIndexStatus({ silent: true })')) {
  failures.push('model provider restoration must refresh neural Embedding index status');
}
if (!appSource.includes("if (startupTarget.route === 'settings') activateSetting(params.get('setting') || 'general', false)")) {
  failures.push('authorized startup must restore the requested settings panel');
}
for (const embeddingCommandScope of [
  "invokeNative('get_neural_embedding_index_status', { vaultId: null })",
  "invokeNative('rebuild_neural_embedding_index', { vaultId: null, consent: true })",
]) {
  if (!appSource.includes(embeddingCommandScope)) failures.push(`neural Embedding workspace scope is missing: ${embeddingCommandScope}`);
}
if (!/\.embedding-index-metrics\s*\{[^}]*grid-template-columns:\s*repeat\(3,\s*minmax\(92px,\s*\.7fr\)\)\s+minmax\(180px,\s*1\.9fr\)/u.test(styles)
  || !/\.embedding-index-status > footer\s*\{[^}]*display:\s*flex[^}]*justify-content:\s*space-between/u.test(styles)) {
  failures.push('neural Embedding status card must preserve its desktop metrics and action layout');
}
if (!/\.embedding-index-progress > span\s*\{[^}]*transition:\s*transform/u.test(styles)
  || /\.embedding-index-progress > span\s*\{[^}]*transition:\s*width/u.test(styles)) {
  failures.push('neural Embedding progress must animate with transform instead of layout width');
}
for (const creationImagePlaceholderPrimitive of [
  'const creationImagePlaceholderSrc =',
  'image.src = creationImagePlaceholderSrc',
  "image.dataset.draftPlaceholder = 'true'",
]) {
  if (!appSource.includes(creationImagePlaceholderPrimitive)) failures.push(`creation draft image placeholder is missing: ${creationImagePlaceholderPrimitive}`);
}
for (const creationRuntimePrimitive of [
  "from './creation/content-type-runtime.js'",
  "from './creation/editor-adapter.js'",
  "from './creation/document-title.js'",
  'evaluateCreationContentTypeRuntime(',
  'function evaluateActiveCreationRuntime(',
  'function buildCreationRenderedHtml(runtime',
  '...runtime.checks',
  "from './creation/html-studio.js'",
  'buildHtmlStudioPreview({ channels: { html: fragment, css: baseCss } })',
  "invokeNative('normalize_creation_document'",
  'creationDocumentToEditorHtml(creationDocument)',
  'editorElementToMarkdown(editor, { attachmentPaths })',
  'resolveCreationDocumentTitle(requested',
  'ensureCreationExportAllowed(',
  'button.disabled = blocked',
  'recordCreationExport(',
]) {
  if (!appSource.includes(creationRuntimePrimitive)) failures.push(`typed creation runtime integration is missing: ${creationRuntimePrimitive}`);
}
for (const creationNativeRegistration of [
  'creation::list_creation_catalog',
  'creation::normalize_creation_document',
]) {
  if (!nativeLibrary.includes(creationNativeRegistration)) failures.push(`native creation command is not registered: ${creationNativeRegistration}`);
}

const runCreationRewriteSource = extractJavaScriptFunction(appSource, 'runCreationRewrite');
const cancelCreationWritingTransportSource = extractJavaScriptFunction(appSource, 'cancelCreationWritingTransport');
const acceptCreationRewriteSource = extractJavaScriptFunction(appSource, 'acceptCreationRewrite');
const assertCreationRewriteStillCurrentSource = extractJavaScriptFunction(appSource, 'assertCreationRewriteStillCurrent');
const normalizeCreationDocumentForRuntimeSource = extractJavaScriptFunction(appSource, 'normalizeCreationDocumentForRuntime');
const saveCreationToVaultSource = extractJavaScriptFunction(appSource, 'saveCreationToVault');

const createWritingRunLocal = importedLocalName(appSource, './creation/writing-panel.js', 'createWritingRun');
if (!createWritingRunLocal) {
  failures.push('creation rewrite must import createWritingRun from writing-panel.js');
} else {
  if (!/\bexport\s+async\s+function\s+createWritingRun\s*\(/u.test(writingPanelSource)) {
    failures.push('writing-panel.js must export createWritingRun');
  }
  if (!callsIdentifier(runCreationRewriteSource, createWritingRunLocal, { awaited: true })) {
    failures.push('creation rewrite must await createWritingRun before starting execution');
  }
}

const createExecutionControllerLocal = importedLocalName(appSource, './creation/execution-controller.js', 'createCreationExecutionController');
const restoreExecutionControllerLocal = importedLocalName(appSource, './creation/execution-controller.js', 'restoreCreationExecutionController');
if (!createExecutionControllerLocal || !restoreExecutionControllerLocal) {
  failures.push('creation rewrite must import createCreationExecutionController and restoreCreationExecutionController from execution-controller.js');
} else {
  if (!/\bexport\s+function\s+restoreCreationExecutionController\s*\(/u.test(executionControllerSource)) {
    failures.push('execution-controller.js must export restoreCreationExecutionController');
  }
  if (!callsIdentifier(runCreationRewriteSource, createExecutionControllerLocal)) {
    failures.push('new WritingRuns must be owned by createCreationExecutionController');
  }
  if (!callsIdentifier(runCreationRewriteSource, restoreExecutionControllerLocal)) {
    failures.push('recoverable WritingRuns must use restoreCreationExecutionController');
  }
}

if (!/\babort\s*:\s*\(?\s*reason\s*\)?\s*=>\s*cancelCreationWritingTransport\s*\(/u.test(runCreationRewriteSource)) {
  failures.push('creation execution controllers must route aborts through cancelCreationWritingTransport');
}
if (!invokesNativeCommand(cancelCreationWritingTransportSource, 'cancel_assistant_request')) {
  failures.push('creation cancellation must invoke the native cancel_assistant_request command');
}
if (!registersRustCommand(nativeLibrary, 'model_provider', 'cancel_assistant_request')) {
  failures.push('native cancel_assistant_request command is not registered');
}
if (!/#\s*\[\s*tauri::command\s*\]\s*pub\s+(?:async\s+)?fn\s+cancel_assistant_request\s*\(/u.test(modelProvider)
  || !/\bpub\s+(?:async\s+)?fn\s+cancel_assistant_request\s*\([\s\S]{0,600}?\brequest_state\s*\.\s*cancel\s*\(\s*request_id\s*\)/u.test(modelProvider)) {
  failures.push('native cancel_assistant_request must cancel the tracked model request');
}

if (!invokesNativeCommand(normalizeCreationDocumentForRuntimeSource, 'validate_creation_document')) {
  failures.push('typed Creation normalization must invoke native validate_creation_document when requested');
}
if (!registersRustCommand(nativeLibrary, 'creation', 'validate_creation_document')) {
  failures.push('native validate_creation_document command is not registered');
}
if (!/#\s*\[\s*tauri::command\s*\]\s*pub\s+fn\s+validate_creation_document\s*\([^)]*\)\s*->\s*ValidationReport\s*\{\s*validate_document\s*\(\s*&\s*document\s*\)\s*\}/u.test(nativeCreation)) {
  failures.push('native validate_creation_document must delegate to Creation validate_document');
}

const prepareDurableTextNoteWriteLocal = importedLocalName(appSource, './creation/durable-assets.js', 'prepareDurableTextNoteWrite');
if (!prepareDurableTextNoteWriteLocal
  || !new RegExp(`\\bawait\\s+${escapeRegularExpression(prepareDurableTextNoteWriteLocal)}\\s*\\(`, 'u').test(appSource)) {
  failures.push('creation Vault save must stage the complete Markdown body through prepareDurableTextNoteWrite');
}
if (!/\bexport\s+async\s+function\s+prepareDurableTextNoteWrite\s*\(/u.test(durableAssetsSource)
  || !/invoke\(\s*['"]prepare_note_write_from_durable_asset['"]/u.test(durableAssetsSource)
  || !/invoke\(\s*['"]delete_durable_asset['"]/u.test(durableAssetsSource)) {
  failures.push('durable Creation Vault staging must prepare from the asset and clean it when preparation fails');
}
if (!registersRustCommand(nativeLibrary, 'obsidian', 'prepare_note_write_from_durable_asset')) {
  failures.push('native prepare_note_write_from_durable_asset command is not registered');
}
if (!obsidianAdapter.includes('Self::Durable(path) => hash_file_streaming(path)')
  || !obsidianAdapter.includes('Self::Durable(path) => BatchFileSource::Path(path)')
  || !obsidianAdapter.includes('pending.batch_source()')
  || !obsidianAdapter.includes('vault_batch::commit_batch_sources(')
  || !vaultBatch.includes('Self::Path(path) => hash_file(path)')
  || !vaultBatch.includes('let mut buffer = vec![0u8; 1024 * 1024];')
  || !vaultBatch.includes('BatchFileSource::Path(source_path) => durable_atomic_copy(path, source_path)')) {
  failures.push('native Vault adapter must hash and commit durable note bodies through bounded file streaming');
}
if (/\binvokeNative\s*\(\s*['"]prepare_note_write['"]/u.test(saveCreationToVaultSource)) {
  failures.push('creation Vault save must not send the complete article through one prepare_note_write IPC argument');
}

if (!acceptCreationRewriteSource) {
  failures.push('creation candidate acceptance function is missing');
} else {
  const inputHashCheck = assertCreationRewriteStillCurrentSource.search(
    /\bawait\s+sha256Text\s*\(\s*draft\s*\.\s*original\s*\)\s*!==\s*run\s*\.\s*inputHash\b/u,
  );
  const outputHashCheck = assertCreationRewriteStillCurrentSource.search(
    /\bawait\s+sha256Text\s*\(\s*draft\s*\.\s*revised\s*\)\s*!==\s*run\s*\.\s*outputHash\b/u,
  );
  const freshnessCall = acceptCreationRewriteSource.search(/\bawait\s+assertCreationRewriteStillCurrent\s*\(/u);
  const nativeValidationCall = acceptCreationRewriteSource.search(
    /\bawait\s+normalizeCreationDocumentForRuntime\s*\([\s\S]*?,\s*\{\s*validate\s*:\s*true\s*\}\s*\)/u,
  );
  const nativeValidationGuard = acceptCreationRewriteSource.search(
    /runtimeCandidate\s*\.\s*validation\s*&&\s*!\s*runtimeCandidate\s*\.\s*validation\s*\.\s*valid\b/u,
  );
  const candidateCommit = acceptCreationRewriteSource.search(/\bcommitCreationEditorHistory\s*\(/u);
  if (!assertCreationRewriteStillCurrentSource || inputHashCheck < 0 || outputHashCheck < 0) {
    failures.push('creation candidate acceptance must compare candidate content with run.inputHash and run.outputHash');
  } else if (candidateCommit < 0 || freshnessCall < 0 || freshnessCall > candidateCommit) {
    failures.push('WritingRun input/output hashes must be verified before the creation candidate is committed');
  }
  if (nativeValidationCall < 0 || nativeValidationGuard < 0) {
    failures.push('creation candidate acceptance must request and enforce native Creation validation');
  } else if (candidateCommit < 0 || nativeValidationCall > candidateCommit || nativeValidationGuard > candidateCommit) {
    failures.push('native Creation validation must pass before the creation candidate is committed');
  }
}
for (const embeddingNativeRegistration of [
  'runtime_db::get_neural_embedding_index_status',
  'runtime_db::rebuild_neural_embedding_index',
]) {
  if (!nativeLibrary.includes(embeddingNativeRegistration)) failures.push(`neural Embedding command is not registered: ${embeddingNativeRegistration}`);
}
for (const embeddingRuntimePrimitive of [
  'pub struct NeuralEmbeddingIndexStatus',
  'CREATE TABLE IF NOT EXISTS neural_embedding_cache',
  'CREATE TABLE IF NOT EXISTS note_neural_embeddings',
  'CREATE TABLE IF NOT EXISTS neural_embedding_index_state',
  'pub fn get_neural_embedding_index_status(',
  'pub async fn rebuild_neural_embedding_index(',
]) {
  if (!runtimeDatabase.includes(embeddingRuntimePrimitive)) failures.push(`neural Embedding runtime primitive is missing: ${embeddingRuntimePrimitive}`);
}
for (const requiredInteraction of [
  "document.querySelectorAll('[data-route]')",
  "document.querySelectorAll('[data-tab]')",
  "document.getElementById('r10-task-drawer-trigger')",
  "document.addEventListener('keydown'",
  "toggleAttribute('inert'",
]) {
  if (!appSource.includes(requiredInteraction)) failures.push(`core interaction handler missing: ${requiredInteraction}`);
}

const routedClickPreamble = appSource.match(
  /const button = event\.target\.closest\('button'\);[\s\S]*?const view = button\.closest\('\[data-view\]'\)\?\.dataset\.view;/u,
)?.[0] || '';
if (!/button\.closest\('\[data-conversation-list\] \[data-conversation-id\]'\)/u.test(routedClickPreamble)) {
  failures.push('global conversation selector must run before data-view click dispatch');
}
if (!routedClickPreamble.includes('selectSecretaryConversation(')) {
  failures.push('global conversation click must select the requested conversation before data-view dispatch');
}

for (const hybridSearchPrimitive of [
  'function mergeKnowledgeSearchResults(',
  'function compareKnowledgeSearchResults(',
  'function indexedKnowledgeSignalBonus(',
  'result?.rankingSignals',
  "add(indexedItems, 'hybrid')",
  '1000 + (Number.isFinite(score) ? score : 0)',
]) {
  if (!appSource.includes(hybridSearchPrimitive)) {
    failures.push(`hybrid search frontend ranking is missing: ${hybridSearchPrimitive}`);
  }
}
if (appSource.includes('Math.round(rawScore)')) {
  failures.push('hybrid RRF score is still rounded to an integer');
}
if (!/\.view\s*\{[^}]*overflow-y:\s*auto/isu.test(styles)) failures.push('page-level vertical scrolling is missing');
if (!/\.task-drawer\s*\{[^}]*position:\s*fixed/isu.test(styles)) failures.push('task drawer must remain a fixed overlay');
if (!/\.drawer-section\s*\{[^}]*overflow-y:\s*auto/isu.test(styles)) failures.push('task drawer content scrolling is missing');

const onboardingSteps = [...html.matchAll(/\bdata-onboarding-step="([0-2])"/gu)].map((match) => match[1]);
if (onboardingSteps.join(',') !== '0,1,2') failures.push(`first-run onboarding must contain exactly three ordered features; found ${onboardingSteps.join(',') || 'none'}`);
for (const onboardingCopy of ['从你的知识开始', '回到上次停下的地方', '需要时再打开助手']) {
  if (!html.includes(onboardingCopy)) failures.push(`first-run onboarding is missing: ${onboardingCopy}`);
}
if (!appSource.includes('const ONBOARDING_VERSION = 2')) failures.push('current versioned first-run onboarding state is missing');
if (!appSource.includes("if (!openOnboarding()) openAssistantSetup()")) failures.push('first launch must open onboarding before assistant preferences');
const avatarOptions = [...html.matchAll(/\bdata-assistant-avatar="([^"]+)"/gu)].map((match) => match[1]);
if (avatarOptions.length < 8) failures.push(`assistant Lucide icon picker must include at least 8 built-in choices; found ${avatarOptions.length}`);
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
const prepareCaptureImageCommand = capturePipeline.match(/pub fn prepare_capture_image_analysis_input\([\s\S]*?\n\}/u)?.[0] || '';
if (prepareCaptureImageCommand.includes('return capture_image_analysis_input_with_adapter(')) {
  failures.push('Windows image preparation command must use its cfg block as the tail expression');
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
if (!mediaDiscovery.includes('".avi"')) failures.push('Windows-verified AVI input is missing from media discovery suffixes');
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
for (const windowsSignatureProbePrimitive of ["runJson('pwsh.exe'", '$ErrorActionPreference = "Stop"', '-ErrorAction Stop', 'Authenticode signature probe returned no result', 'ConvertTo-Json -InputObject ([string]$status)', 'rejectStderr: true']) {
  if (!windowsReleaseVerifier.includes(windowsSignatureProbePrimitive)) {
    failures.push(`Windows Authenticode probe is not fail-closed: ${windowsSignatureProbePrimitive}`);
  }
}
if (!windowsReleaseVerifier.includes("join(root, 'src-tauri', 'target', '云枢-Windows-安装冒烟')")) {
  failures.push('Windows NSIS smoke install directory must preserve Unicode without spaces in the /D argument');
}
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
  '当前不建立实体图谱',
  '统一索引链维护本地特征向量与 RRF 混合检索',
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
  'EXPECTED_LICENSE_SHA256',
  '62bec384df47b0328307db41455ff6ea2559e5546b394ac69148561b21703120',
  'YUNSPIRE_RUNTIME.json',
  'python313._pth',
  'LICENSE.txt',
  'licenseSha256',
  'await smokeTest()',
]) {
  if (!windowsPythonBuild.includes(runtimePrimitive)) failures.push(`Windows embedded Python runtime gate is missing: ${runtimePrimitive}`);
}
for (const verifierPrimitive of [
  'EXPECTED_PYTHON_LICENSE_SHA256',
  '62bec384df47b0328307db41455ff6ea2559e5546b394ac69148561b21703120',
  'installedRuntimeManifest.licenseSha256 !== EXPECTED_PYTHON_LICENSE_SHA256',
  'await sha256(installedRuntimeLicense) !== EXPECTED_PYTHON_LICENSE_SHA256',
]) {
  if (!windowsReleaseVerifier.includes(verifierPrimitive)) {
    failures.push(`Windows CPython license verification is missing: ${verifierPrimitive}`);
  }
}
if (!capturePipeline.includes('yunspire-runtime') || !capturePipeline.includes('python.exe')) {
  failures.push('Windows packaged Python runtime is not resolved by the native capture pipeline');
}
const manifestDirectoryReferences = capturePipeline.match(/env!\("CARGO_MANIFEST_DIR"\)/gu)?.length || 0;
const debugGatedManifestDirectoryReferences = capturePipeline
  .match(/#\[cfg\(debug_assertions\)\][\s\S]{0,400}?env!\("CARGO_MANIFEST_DIR"\)/gu)?.length || 0;
if (manifestDirectoryReferences === 0
  || manifestDirectoryReferences !== debugGatedManifestDirectoryReferences) {
  failures.push(`CARGO_MANIFEST_DIR must be compiled only in debug builds: ${debugGatedManifestDirectoryReferences}/${manifestDirectoryReferences}`);
}
const windowsResources = windowsTauri.bundle?.resources || {};
for (const [source, destination] of [
  ['target/yunspire-runtime/python/', 'runtime/python/'],
  ['target/yunspire-native/yunspire_pdf_windows.exe', 'skills/document-content-analysis/scripts/yunspire_pdf_windows.exe'],
  ['target/yunspire-native/yunspire_image_windows.exe', 'skills/document-content-analysis/scripts/yunspire_image_windows.exe'],
]) {
  if (windowsResources[source] !== destination) failures.push(`Windows native bundle resource mapping is missing: ${source}`);
}
const legalResources = [
  ['../LICENSE', 'legal/LICENSE'],
  ['../NOTICE', 'legal/NOTICE'],
  ['target/yunspire-licenses/THIRD_PARTY_NOTICES.txt', 'legal/THIRD_PARTY_NOTICES.txt'],
];
if (tauri.bundle?.licenseFile !== '../LICENSE') failures.push('Tauri bundle licenseFile must use the Yunspire source license');
for (const [source, destination] of legalResources) {
  if (tauri.bundle?.resources?.[source] !== destination) failures.push(`Main legal bundle resource mapping is missing: ${source}`);
  if (windowsResources[source] !== destination) failures.push(`Windows legal bundle resource mapping is missing: ${source}`);
}
if (!packageConfig.scripts?.build?.includes('generate-third-party-notices.mjs')) {
  failures.push('production build must generate third-party notices before bundling');
}
for (const primitive of [
  'package-lock.json',
  'cargo',
  '--locked',
  '--filter-platform',
  'YUNSPIRE_LICENSE_TARGETS',
  'package-lock.json 包路径越过 node_modules',
  'cargoLockChecksums',
  'licenseNamePattern',
  'reviewedTextlessPackages',
  '未命中版本与哈希审查白名单',
  'npmPackageHasPlatformConstraint',
  '锁定的 npm 包未安装',
  'npm 安装内容与锁文件不一致',
  'THIRD_PARTY_NOTICES.txt',
  'Reviewed Packages Without an Upstream License File',
]) {
  if (!licenseGenerator.includes(primitive)) failures.push(`third-party notice generator is missing: ${primitive}`);
}
if (thirdPartyNotices.length < 100_000
  || !thirdPartyNotices.includes('Yunspire Third-Party Notices')
  || !thirdPartyNotices.includes('Package Inventory')
  || !thirdPartyNotices.includes('Reviewed Packages Without an Upstream License File')
  || !thirdPartyNotices.includes('integrity: sha256-')
  || !thirdPartyNotices.includes('integrity: sha512-')
  || !thirdPartyNotices.includes('Bundled License Texts')
  || !thirdPartyNotices.includes('MPL-2.0')) {
  failures.push('generated third-party notices are missing or incomplete');
}
if (/\/Users\/[^/\s]+\/|[A-Za-z]:\\Users\\/u.test(thirdPartyNotices)) {
  failures.push('generated third-party notices expose an absolute user path');
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
for (const modelUnderstandingPromptPrimitive of [
  'analysis_markdown 不是简短摘要',
  'image_observations 每项必须返回 asset_id、reference_id',
  '它不是实体图谱',
]) {
  if (!captureAnalysisSystemPrompt.includes(modelUnderstandingPromptPrimitive)) failures.push(`model-understood source contract is missing: ${modelUnderstandingPromptPrimitive}`);
}
for (const modelUnderstandingPrimitive of [
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
const embeddedLinkNormalizer = extractJavaScriptFunction(embeddedLinkRuntime, 'normalizedCapturedEmbeddedLinks');
const embeddedLinkHydration = extractJavaScriptFunction(appSource, 'hydrateEmbeddedLinkCaptureParameters');
if (/\.slice\(\s*0,\s*(?:128|512)\b/u.test(`${embeddedLinkNormalizer}\n${embeddedLinkHydration}`)) {
  failures.push('file-internal links still have a silent 128 or 512 item cap');
}
if (!appSource.includes('embedded_link_occurrences') || !appSource.includes('partitionDeterministicCaptureRequests')) failures.push('file-internal link occurrence batching is missing');
if (!embeddedLinkRuntime.includes('for (let offset = 0; offset < requests.length; offset += normalizedBatchSize)')) {
  failures.push('file-internal link requests are not partitioned without an aggregate cap');
}
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
for (const command of requiredSlashCommands) {
  if (!assistantSlashCommandPrompt.includes(`/${command}`)) failures.push(`assistant slash-command prompt is missing /${command}`);
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
