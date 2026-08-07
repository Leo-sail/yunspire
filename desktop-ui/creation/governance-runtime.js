function assertInvoke(invoke) {
  if (typeof invoke !== 'function') throw new TypeError('创作治理运行时缺少命令调用器');
}

function assertObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} 必须是对象`);
  return value;
}

function assertList(value, label) {
  if (!Array.isArray(value)) throw new TypeError(`${label} 必须是列表`);
  return value;
}

function identifier(value, label) {
  const normalized = String(value || '').trim();
  if (!/^[a-z][a-z0-9-]{0,79}$/u.test(normalized)) throw new TypeError(`${label} 无效`);
  return normalized;
}

function positiveRevision(value, label) {
  const revision = Number(value);
  if (!Number.isSafeInteger(revision) || revision < 1) throw new TypeError(`${label} 无效`);
  return revision;
}

function resourceType(value) {
  const normalized = String(value || '').trim();
  if (!['theme', 'component', 'template'].includes(normalized)) throw new TypeError('创作资源类型无效');
  return normalized;
}

function assertResourceRecord(value, label = '创作资源回执') {
  const record = assertObject(value, label);
  resourceType(record.resourceType ?? record.resource_type);
  identifier(record.id, '创作资源 ID');
  positiveRevision(record.revision, '创作资源 revision');
  if (!['active', 'archived'].includes(String(record.state || ''))) throw new TypeError('创作资源状态无效');
  if (!/^sha256:[a-f0-9]{64}$/u.test(String(record.contentHash ?? record.content_hash ?? ''))) {
    throw new TypeError('创作资源 contentHash 无效');
  }
  return record;
}

function assertResourceArchiveReceipt(value) {
  const receipt = assertObject(value, '创作资源归档回执');
  resourceType(receipt.resourceType ?? receipt.resource_type);
  identifier(receipt.id, '创作资源 ID');
  positiveRevision(receipt.revision, '创作资源 revision');
  if (receipt.state !== 'archived') throw new TypeError('创作资源归档状态无效');
  if (!/^sha256:[a-f0-9]{64}$/u.test(String(receipt.contentHash ?? receipt.content_hash ?? ''))) {
    throw new TypeError('创作资源归档 contentHash 无效');
  }
  return receipt;
}

function assertBrandRecord(value, label = 'BrandProfile 回执') {
  const record = assertObject(value, label);
  const profile = assertObject(record.profile, 'BrandProfile');
  identifier(profile.id, 'BrandProfile ID');
  positiveRevision(record.revision, 'BrandProfile revision');
  if (!['draft', 'active', 'archived'].includes(String(record.status || ''))) throw new TypeError('BrandProfile 状态无效');
  if (!/^sha256:[a-f0-9]{64}$/u.test(String(record.contentHash ?? record.content_hash ?? ''))) {
    throw new TypeError('BrandProfile contentHash 无效');
  }
  return record;
}

function normalizeCsv(values) {
  const list = Array.isArray(values) ? values : String(values || '').split(/[，,\n]/u);
  return [...new Set(list.map((value) => String(value || '').trim()).filter(Boolean))];
}

export function createDefaultBrandProfile({ id = 'brand-default', name = '默认品牌', now = new Date().toISOString() } = {}) {
  const profileId = identifier(id, 'BrandProfile ID');
  const profileName = String(name || '').trim();
  if (!profileName || profileName.length > 120) throw new TypeError('BrandProfile 名称无效');
  return {
    schemaVersion: '1.0',
    id: profileId,
    revision: 1,
    name: profileName,
    description: null,
    status: 'draft',
    voice: { presetId: null, traits: ['清晰', '可信'], prohibitedTraits: [] },
    vocabulary: { preferred: [], avoided: [], requiredTerms: [] },
    style: {
      formality: 'balanced',
      perspective: 'mixed',
      sentenceLength: 'varied',
      emoji: 'none',
      punctuation: 'standardCjk',
      callToAction: 'none',
    },
    claimsPolicy: {
      requireSources: true,
      labelInference: true,
      forbidFabrication: true,
      sensitiveTopics: [],
    },
    purposeDefaults: {},
    signature: { enabled: false, author: null, byline: null, footer: null },
    examples: [],
    provenance: { createdBy: 'user', source: 'manual', sourceRef: null, userApproved: false },
    createdAt: now,
    updatedAt: now,
  };
}

export function normalizeBrandProfileDraft(value) {
  const profile = structuredClone(assertObject(value, 'BrandProfile'));
  profile.schemaVersion = '1.0';
  profile.id = identifier(profile.id, 'BrandProfile ID');
  profile.name = String(profile.name || '').trim();
  if (!profile.name || profile.name.length > 120) throw new TypeError('BrandProfile 名称无效');
  profile.voice = assertObject(profile.voice, 'BrandProfile voice');
  profile.voice.traits = normalizeCsv(profile.voice.traits);
  if (!profile.voice.traits.length) throw new TypeError('BrandProfile voice.traits 至少需要一项');
  profile.voice.prohibitedTraits = normalizeCsv(profile.voice.prohibitedTraits);
  profile.vocabulary = assertObject(profile.vocabulary, 'BrandProfile vocabulary');
  profile.vocabulary.requiredTerms = normalizeCsv(profile.vocabulary.requiredTerms);
  profile.claimsPolicy = assertObject(profile.claimsPolicy, 'BrandProfile claimsPolicy');
  profile.claimsPolicy.forbidFabrication = true;
  profile.provenance = assertObject(profile.provenance, 'BrandProfile provenance');
  profile.provenance.userApproved = false;
  profile.status = 'draft';
  return profile;
}

export function createCreationGovernanceRuntime(invoke) {
  assertInvoke(invoke);
  return Object.freeze({
    async listResources({ includeArchived = true } = {}) {
      const records = assertList(await invoke('list_creation_resources', { includeArchived }), '创作资源列表');
      return records.map((record) => assertResourceRecord(record));
    },

    async listResourceRevisions(record) {
      const resource = assertResourceRecord(record, '当前创作资源');
      const records = assertList(await invoke('list_creation_resource_revisions', {
        resourceType: resourceType(resource.resourceType ?? resource.resource_type),
        id: identifier(resource.id, '创作资源 ID'),
      }), '创作资源版本列表');
      return records.map((item) => assertResourceRecord(item, '创作资源版本'));
    },

    async restoreResource(record, revision) {
      const resource = assertResourceRecord(record, '当前创作资源');
      return assertResourceRecord(await invoke('restore_creation_resource_revision', {
        input: {
          resourceType: resourceType(resource.resourceType ?? resource.resource_type),
          id: identifier(resource.id, '创作资源 ID'),
          revision: positiveRevision(revision, '待恢复 revision'),
          expectedCurrentRevision: positiveRevision(resource.revision, '当前 revision'),
        },
      }), '创作资源恢复回执');
    },

    async archiveResource(record) {
      const resource = assertResourceRecord(record, '当前创作资源');
      return assertResourceArchiveReceipt(await invoke('archive_creation_resource', {
        resourceType: resourceType(resource.resourceType ?? resource.resource_type),
        id: identifier(resource.id, '创作资源 ID'),
      }));
    },

    async listBrandProfiles({ includeArchived = true } = {}) {
      const records = assertList(await invoke('list_creation_brand_profiles', { includeArchived }), 'BrandProfile 列表');
      return records.map((record) => assertBrandRecord(record));
    },

    async upsertBrandProfile(profile, expectedRevision = null) {
      const draft = normalizeBrandProfileDraft(profile);
      const input = { profile: draft };
      if (expectedRevision !== null && expectedRevision !== undefined) {
        input.expectedRevision = positiveRevision(expectedRevision, 'BrandProfile expectedRevision');
      }
      return assertBrandRecord(await invoke('upsert_creation_brand_profile', { input }));
    },

    async approveBrandProfile(record) {
      const brand = assertBrandRecord(record);
      return assertBrandRecord(await invoke('approve_creation_brand_profile', {
        profileId: identifier(brand.profile.id, 'BrandProfile ID'),
        expectedRevision: positiveRevision(brand.revision, 'BrandProfile revision'),
      }));
    },

    async archiveBrandProfile(record) {
      const brand = assertBrandRecord(record);
      return assertBrandRecord(await invoke('archive_creation_brand_profile', {
        profileId: identifier(brand.profile.id, 'BrandProfile ID'),
        expectedRevision: positiveRevision(brand.revision, 'BrandProfile revision'),
      }));
    },

    async deleteBrandProfile(record) {
      const brand = assertBrandRecord(record);
      return assertBrandRecord(await invoke('delete_creation_brand_profile', {
        profileId: identifier(brand.profile.id, 'BrandProfile ID'),
        expectedRevision: positiveRevision(brand.revision, 'BrandProfile revision'),
      }));
    },

    async bindBrandProfile(document, profileId = null) {
      const currentDocument = assertObject(document, '待绑定文稿');
      const expectedDocumentRevision = positiveRevision(currentDocument.revision, '文稿 revision');
      const normalizedProfileId = profileId === null || String(profileId).trim() === ''
        ? null
        : identifier(profileId, 'BrandProfile ID');
      const receipt = assertObject(await invoke('bind_creation_brand_profile', {
        input: { document: currentDocument, profileId: normalizedProfileId, expectedDocumentRevision },
      }), 'BrandProfile 绑定回执');
      assertObject(receipt.document, 'BrandProfile 绑定文稿回执');
      if (receipt.profile !== null && receipt.profile !== undefined) assertBrandRecord(receipt.profile);
      return receipt;
    },

    async evaluateBrandProfile(document, profileId = null) {
      const currentDocument = assertObject(document, '待评测文稿');
      const normalizedProfileId = profileId === null || String(profileId).trim() === ''
        ? null
        : identifier(profileId, 'BrandProfile ID');
      const result = assertObject(await invoke('evaluate_creation_brand_profile', {
        input: { document: currentDocument, profileId: normalizedProfileId },
      }), 'BrandProfile 评测回执');
      if (!Array.isArray(result.checks)) throw new TypeError('BrandProfile 评测检查项无效');
      if (!Number.isFinite(Number(result.score))) throw new TypeError('BrandProfile 评测分数无效');
      return result;
    },
  });
}
