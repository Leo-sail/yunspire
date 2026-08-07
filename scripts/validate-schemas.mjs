import { readFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { basename, dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import Ajv2020 from "ajv/dist/2020.js";
import addFormats from "ajv-formats";

const projectRoot = resolve(fileURLToPath(new URL("..", import.meta.url)));
const schemaDirectories = [
  resolve(projectRoot, "docs/schemas"),
  resolve(projectRoot, "skills/beautify-markdown"),
  resolve(projectRoot, "skills/deep-research"),
];
const manifestDirectories = [
  resolve(projectRoot, "resources/creation/themes"),
  resolve(projectRoot, "resources/creation/components"),
  resolve(projectRoot, "resources/creation/templates"),
];
const creationCatalogPath = resolve(projectRoot, "resources/creation/catalog/creation-catalog.json");
const creationRuntimeBundlePath = resolve(projectRoot, "resources/creation/catalog/runtime-bundle.json");
const writingResourcesPath = resolve(projectRoot, "resources/creation/catalog/writing-resources.json");
const expectedCreationCounts = Object.freeze({ themes: 85, components: 53, templates: 75 });
const preservedFirstPartyIds = Object.freeze({
  themes: ["ink", "jade", "vermilion", "graphite"],
  components: ["lead", "quote", "notice", "steps", "metrics", "compare", "dialogue", "timeline", "divider", "cta"],
});

function projectPath(filePath) {
  return relative(projectRoot, filePath).replaceAll("\\", "/");
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function readJson(filePath) {
  try {
    return JSON.parse(await readFile(filePath, "utf8"));
  } catch (error) {
    throw new Error(`Invalid JSON in ${projectPath(filePath)}: ${error.message}`);
  }
}

async function readOptionalDirectory(directory) {
  try {
    return await readdir(directory);
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
}

async function walkFiles(directory) {
  try {
    const entries = await readdir(directory, { withFileTypes: true });
    return (await Promise.all(entries.map(async (entry) => {
      const entryPath = resolve(directory, entry.name);
      if (entry.isDirectory()) return walkFiles(entryPath);
      return [entryPath];
    }))).flat();
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw error;
  }
}

function normalizedResourceText(value) {
  return String(value || "").replace(/\s+/gu, " ").trim();
}

function assertMeaningfulText(value, label, minimum = 3) {
  const normalized = normalizedResourceText(value);
  assert(normalized.length >= minimum, `${label} must contain editable content`);
  assert(!/^(?:theme|component|template|resource|主题|组件|模板|资源)[-_ ]*\d+$/iu.test(normalized), `${label} cannot be a numbered placeholder`);
  return normalized;
}

function assertPathInside(directory, filePath, label) {
  const pathFromDirectory = relative(directory, filePath);
  assert(pathFromDirectory && !pathFromDirectory.startsWith("..") && !pathFromDirectory.includes("../"), `${label} must stay within ${projectPath(directory)}`);
}

function sha256(value) {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

const schemaPaths = (await Promise.all(schemaDirectories.map(async (directory) =>
  (await readdir(directory))
    .filter((name) => name.endsWith(".schema.json"))
    .map((name) => resolve(directory, name))))).flat();

const ajv = new Ajv2020({ allErrors: true, strict: true });
addFormats(ajv);

const schemaEntries = await Promise.all(schemaPaths.map(async (schemaPath) => ({
  path: schemaPath,
  schema: await readJson(schemaPath),
})));
const schemaIds = new Set();
for (const entry of schemaEntries) {
  const { path, schema } = entry;
  entry.key = typeof schema.$id === "string" && schema.$id.length > 0 ? schema.$id : projectPath(path);
  assert(!schemaIds.has(entry.key), `Duplicate schema id or path: ${entry.key}`);
  schemaIds.add(entry.key);
  ajv.addSchema(schema, entry.key);
}

for (const { path, key } of schemaEntries) {
  assert(ajv.getSchema(key), `Unable to compile ${projectPath(path)}`);
}

const manifestPaths = (await Promise.all(manifestDirectories.map(async (directory) =>
  (await readOptionalDirectory(directory))
    .filter((name) => name.endsWith(".manifest.json"))
    .map((name) => resolve(directory, name))))).flat();
const manifestSchemaIds = {
  theme: "https://yunspire.local/schemas/theme-manifest.schema.json",
  component: "https://yunspire.local/schemas/component-manifest.schema.json",
  template: "https://yunspire.local/schemas/template-manifest.schema.json",
};
const manifestEntries = await Promise.all(manifestPaths.map(async (manifestPath) => ({
  path: manifestPath,
  projectPath: projectPath(manifestPath),
  manifest: await readJson(manifestPath),
})));

for (const entry of manifestEntries) {
  const schemaId = manifestSchemaIds[entry.manifest.manifestType];
  assert(schemaId, `${entry.projectPath} has unknown manifestType ${entry.manifest.manifestType}`);
  const validate = ajv.getSchema(schemaId);
  assert(validate, `Missing manifest schema ${schemaId}`);
  assert(validate(entry.manifest), `${entry.projectPath} failed schema validation:\n${ajv.errorsText(validate.errors, { separator: "\n" })}`);
  assert(basename(entry.path, ".manifest.json") === entry.manifest.id, `${entry.projectPath} filename must match manifest id ${entry.manifest.id}`);
  assertMeaningfulText(entry.manifest.displayName, `${entry.projectPath} displayName`, 1);
  assertMeaningfulText(entry.manifest.description, `${entry.projectPath} description`, 12);
  if (entry.manifest.manifestType === "component") {
    assertMeaningfulText(entry.manifest.templateMarkdown, `${entry.projectPath} templateMarkdown`);
  }
}

const catalog = await readJson(creationCatalogPath);
assert(catalog.schemaVersion === "1.0", "Creation catalog schemaVersion must be 1.0");
assert(/^\d+\.\d+\.\d+$/.test(catalog.catalogVersion), "Creation catalog must use a semantic catalogVersion");
assert(catalog.status === "active", "Complete creation catalog status must be active");
assert(catalog.source?.policy === "yunspire_first_party", "Creation catalog must declare first-party source policy");
assert(catalog.source?.upstreamCodeCopied === false, "Creation catalog cannot bundle upstream source code");
assert(catalog.source?.upstreamPromptsCopied === false, "Creation catalog cannot bundle upstream prompts");
assert(catalog.source?.upstreamAssetsCopied === false, "Creation catalog cannot bundle upstream assets");
assert(catalog.license?.scope === "yunspire_first_party_project_asset", "Creation catalog must declare its license boundary");
assert(Array.isArray(catalog.researchBoundaries), "Creation catalog must declare research boundaries");
assert(catalog.researchBoundaries.every((item) => item.use === "capabilityResearchOnly" && item.bundled === false), "Research references must remain unbundled capability research");
assert(catalog.resourceLayout?.writingResources === "resources/creation/catalog/writing-resources.json", "Creation catalog must register the writing resource layout");

const writingResources = await readJson(writingResourcesPath);
const writingResourceCounts = {
  writingPatterns: Array.isArray(writingResources.writingPatterns) ? writingResources.writingPatterns.length : -1,
  voices: Array.isArray(writingResources.voices) ? writingResources.voices.length : -1,
  purposePresets: Array.isArray(writingResources.purposePresets) ? writingResources.purposePresets.length : -1,
};
assert(writingResources.schemaVersion === "1.0", "Writing resource catalog schemaVersion must be 1.0");
assert(writingResourceCounts.writingPatterns === 53, "Writing resource catalog must contain 53 patterns");
assert(writingResourceCounts.voices === 5, "Writing resource catalog must contain 5 voices");
assert(writingResourceCounts.purposePresets === 9, "Writing resource catalog must contain 9 purpose presets");
assert(catalog.resources?.writingResources?.catalog === "resources/creation/catalog/writing-resources.json", "Creation catalog must register writing-resources.json");
for (const [key, count] of Object.entries(writingResourceCounts)) {
  assert(catalog.coverage?.[key]?.planned === count, `Creation catalog ${key} planned count must be ${count}`);
  assert(catalog.coverage?.[key]?.implemented === count, `Creation catalog ${key} implemented count must be ${count}`);
  assert(catalog.resources.writingResources[key] === count, `Creation catalog ${key} registration count must be ${count}`);
}

const manifestByPath = new Map(manifestEntries.map((entry) => [entry.projectPath, entry]));
const catalogKinds = [
  ["themes", "theme"],
  ["components", "component"],
  ["templates", "template"],
];
const catalogPaths = new Set();
const manifestIdsByType = new Map();

for (const [catalogKey, manifestType] of catalogKinds) {
  const resources = catalog.resources?.[catalogKey];
  const coverage = catalog.coverage?.[catalogKey];
  const expectedCount = expectedCreationCounts[catalogKey];
  assert(Array.isArray(resources), `Creation catalog resources.${catalogKey} must be an array`);
  assert(Number.isInteger(coverage?.planned) && coverage.planned >= 0, `Creation catalog coverage.${catalogKey}.planned must be a non-negative integer`);
  assert(Number.isInteger(coverage?.implemented) && coverage.implemented >= 0, `Creation catalog coverage.${catalogKey}.implemented must be a non-negative integer`);
  assert(coverage.implemented === resources.length, `Creation catalog ${catalogKey} implemented count does not match listed resources`);
  assert(coverage.planned === expectedCount, `Creation catalog ${catalogKey} planned count must be ${expectedCount}`);
  assert(coverage.implemented === expectedCount, `Creation catalog ${catalogKey} implemented count must be ${expectedCount}`);
  const directoryManifestCount = manifestEntries.filter((entry) => entry.manifest.manifestType === manifestType).length;
  assert(directoryManifestCount === expectedCount, `Creation ${catalogKey} directory must contain ${expectedCount} manifests`);
  const ids = new Set();
  const displayNames = new Set();
  for (const resource of resources) {
    assert(typeof resource?.id === "string" && typeof resource?.manifest === "string", `Creation catalog ${catalogKey} entry is incomplete`);
    assert(!ids.has(resource.id), `Duplicate ${manifestType} id in creation catalog: ${resource.id}`);
    assert(!catalogPaths.has(resource.manifest), `Duplicate manifest path in creation catalog: ${resource.manifest}`);
    ids.add(resource.id);
    catalogPaths.add(resource.manifest);
    const entry = manifestByPath.get(resource.manifest);
    assert(entry, `Creation catalog references a missing manifest: ${resource.manifest}`);
    assert(entry.manifest.manifestType === manifestType, `${resource.manifest} must be a ${manifestType} manifest`);
    assert(entry.manifest.id === resource.id, `${resource.manifest} id does not match catalog id ${resource.id}`);
    assert(entry.manifest.catalogVersion === catalog.catalogVersion, `${resource.manifest} catalogVersion does not match the catalog`);
    if (manifestType === "template") {
      const expectedCatalogEntrypoint = `resources/creation/templates/${entry.manifest.entrypoint}`;
      assert(resource.entrypoint === expectedCatalogEntrypoint, `${resource.manifest} catalog entrypoint must be ${expectedCatalogEntrypoint}`);
    }
    assert(!displayNames.has(entry.manifest.displayName), `Duplicate ${manifestType} displayName: ${entry.manifest.displayName}`);
    displayNames.add(entry.manifest.displayName);
  }
  for (const requiredId of preservedFirstPartyIds[catalogKey] || []) {
    assert(ids.has(requiredId), `Creation catalog ${catalogKey} must preserve first-party id ${requiredId}`);
  }
  manifestIdsByType.set(manifestType, ids);
}

assert(catalogPaths.size === manifestEntries.length, "Every creation manifest must be listed exactly once in the creation catalog");
for (const entry of manifestEntries) {
  assert(catalogPaths.has(entry.projectPath), `Unlisted creation manifest: ${entry.projectPath}`);
}

const templateDirectory = resolve(projectRoot, "resources/creation/templates");
const templateEntries = manifestEntries.filter((entry) => entry.manifest.manifestType === "template");
const templateDeclaredFiles = new Set();
const templateContentHashes = new Set();
const templateContentTypes = new Set();
for (const entry of templateEntries) {
  const manifest = entry.manifest;
  const expectedEntrypoint = `${manifest.id}/template.md`;
  assert(manifest.entrypoint === expectedEntrypoint, `${entry.projectPath} entrypoint must be ${expectedEntrypoint}`);
  templateContentTypes.add(manifest.contentType);
  const declaredPaths = new Set();
  for (const file of manifest.files) {
    assert(!declaredPaths.has(file.path), `${entry.projectPath} declares duplicate file ${file.path}`);
    declaredPaths.add(file.path);
    const filePath = resolve(dirname(entry.path), file.path);
    assertPathInside(templateDirectory, filePath, `${entry.projectPath} file ${file.path}`);
    const projectFile = projectPath(filePath);
    assert(!templateDeclaredFiles.has(projectFile), `Template file is declared more than once: ${projectFile}`);
    templateDeclaredFiles.add(projectFile);
    let content;
    try {
      content = await readFile(filePath, "utf8");
    } catch (error) {
      throw new Error(`${entry.projectPath} declares missing template file ${file.path}: ${error.message}`);
    }
    assert(file.contentHash === sha256(content.trimEnd()), `${entry.projectPath} contentHash does not match ${file.path}`);
    if (file.path === manifest.entrypoint) {
      assert(file.kind === "markdown", `${entry.projectPath} entrypoint must have kind markdown`);
      const normalizedContent = normalizedResourceText(content);
      assert(normalizedContent.length >= 160, `${entry.projectPath} entrypoint must contain a substantial Markdown template`);
      assert(/^#\s+\S+/mu.test(content), `${entry.projectPath} entrypoint must begin with a level-one Markdown heading`);
      assert(!templateContentHashes.has(normalizedContent), `Duplicate template Markdown content in ${entry.projectPath}`);
      templateContentHashes.add(normalizedContent);
    }
  }
  assert(declaredPaths.has(manifest.entrypoint), `${entry.projectPath} entrypoint must be listed in files`);
}
const templateMarkdownOnDisk = new Set((await walkFiles(templateDirectory))
  .filter((filePath) => filePath.toLowerCase().endsWith(".md"))
  .map(projectPath));
for (const markdownPath of templateMarkdownOnDisk) {
  assert(templateDeclaredFiles.has(markdownPath), `Unlisted template Markdown file: ${markdownPath}`);
}
const knownTemplateFiles = new Set([
  ...templateEntries.map((entry) => entry.projectPath),
  ...templateDeclaredFiles,
]);
for (const filePath of await walkFiles(templateDirectory)) {
  const resourcePath = projectPath(filePath);
  assert(knownTemplateFiles.has(resourcePath), `Unlisted file in template directory: ${resourcePath}`);
}
for (const contentType of ["article", "wechat", "xiaohongshu", "contract", "paper"]) {
  assert(templateContentTypes.has(contentType), `Creation templates must cover contentType ${contentType}`);
}

const componentEntries = manifestEntries.filter((entry) => entry.manifest.manifestType === "component");
const componentTemplateHashes = new Set();
for (const entry of componentEntries) {
  const normalizedTemplate = normalizedResourceText(entry.manifest.templateMarkdown);
  assert(!componentTemplateHashes.has(normalizedTemplate), `Duplicate component templateMarkdown in ${entry.projectPath}`);
  componentTemplateHashes.add(normalizedTemplate);
}

const themeEntries = manifestEntries.filter((entry) => entry.manifest.manifestType === "theme");
const themeTokenHashes = new Set();
for (const entry of themeEntries) {
  const tokenHash = JSON.stringify({
    palette: entry.manifest.palette,
    typography: entry.manifest.typography,
    spacing: entry.manifest.spacing,
    features: entry.manifest.features,
  });
  assert(!themeTokenHashes.has(tokenHash), `Duplicate theme design tokens in ${entry.projectPath}`);
  themeTokenHashes.add(tokenHash);
}

const runtimeBundle = await readJson(creationRuntimeBundlePath);
assert(runtimeBundle.schemaVersion === "1.0", "Creation runtime bundle schemaVersion must be 1.0");
assert(runtimeBundle.catalogVersion === catalog.catalogVersion, "Creation runtime bundle catalogVersion is stale");
assert(runtimeBundle.themes?.length === expectedCreationCounts.themes, "Creation runtime bundle must contain every theme");
assert(runtimeBundle.components?.length === expectedCreationCounts.components, "Creation runtime bundle must contain every component");
assert(runtimeBundle.templates?.length === expectedCreationCounts.templates, "Creation runtime bundle must contain every template");
const runtimeIds = (items) => new Set(items.map((item) => item.id));
for (const [kind, entries] of [["themes", themeEntries], ["components", componentEntries], ["templates", templateEntries]]) {
  const bundledIds = runtimeIds(runtimeBundle[kind]);
  assert(bundledIds.size === entries.length, `Creation runtime bundle has duplicate ${kind}`);
  entries.forEach((entry) => assert(bundledIds.has(entry.manifest.id), `Creation runtime bundle is missing ${kind} ${entry.manifest.id}`));
}
for (const template of runtimeBundle.templates) {
  assert(/^#\s+\S+/mu.test(template.canonicalMarkdown || ""), `Runtime template ${template.id} is missing canonical Markdown`);
  const disk = await readFile(resolve(templateDirectory, template.entrypoint), "utf8");
  assert(template.canonicalMarkdown === disk.trimEnd(), `Runtime template ${template.id} Markdown is stale`);
}
const certifiedThemes = themeEntries.filter((entry) => entry.manifest.compatibility.wechatCertification === "certified").length;
const legacyCompatibleThemes = themeEntries.filter((entry) => entry.manifest.compatibility.wechatCertification === "legacyCompatible").length;
assert(catalog.coverage.themes.wechatCertified === certifiedThemes, "Certified theme count does not match manifests");
assert(catalog.coverage.themes.legacyCompatible === legacyCompatibleThemes, "Legacy-compatible theme count does not match manifests");
assert(catalog.coverage.themes.wechatCertifiedPlanned === 48, "WeChat certification plan must remain 48 themes");
assert(catalog.coverage.themes.wechatCertified === 0, "Themes without publication evidence cannot be marked certified");
assert(themeEntries.filter((entry) => entry.manifest.compatibility.wechatCertification === "candidate").length === 81, "Generated themes must remain certification candidates");

const componentIds = manifestIdsByType.get("component");
for (const entry of themeEntries) {
  for (const componentId of entry.manifest.supportedComponentIds) {
    assert(componentIds.has(componentId), `${entry.projectPath} references unknown component ${componentId}`);
  }
}

console.log(`SCHEMA_OK ${schemaPaths.length}`);
console.log(`CREATION_MANIFEST_OK ${manifestEntries.length}`);
console.log(`CREATION_CATALOG_OK themes=${catalog.coverage.themes.implemented}/${catalog.coverage.themes.planned} components=${catalog.coverage.components.implemented}/${catalog.coverage.components.planned} templates=${catalog.coverage.templates.implemented}/${catalog.coverage.templates.planned}`);
