import { normalizeCreationDocument } from './document.js';

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

export function invalidateGroundedCreation(documentValue, ledgerValue, options = {}) {
  const document = normalizeCreationDocument(documentValue, { compatibilityAliases: false });
  const ledger = isRecord(ledgerValue) ? { ...ledgerValue } : null;
  const properties = document.metadata.properties || {};
  const verified = properties.groundingVerified === true || ledger?.status === 'verified';
  if (!verified) return { invalidated: false, document, ledger };

  const staleAt = typeof options.staleAt === 'string' && Number.isFinite(Date.parse(options.staleAt))
    ? new Date(options.staleAt).toISOString()
    : new Date().toISOString();
  const reason = String(options.reason || 'contentChanged').slice(0, 120);
  return {
    invalidated: true,
    ledger: {
      ...(ledger || {}),
      status: 'stale',
      staleAt,
      staleReason: reason,
    },
    document: normalizeCreationDocument({
      ...document,
      readiness: null,
      metadata: {
        ...document.metadata,
        properties: {
          ...properties,
          groundingVerified: false,
          groundingStatus: 'stale',
          groundingStaleAt: staleAt,
          groundingStaleReason: reason,
        },
      },
      validationReceipt: {
        ...document.validationReceipt,
        htmlValid: false,
        contentHash: null,
      },
    }, { compatibilityAliases: false }),
  };
}

export function groundedCreationIsCurrent(documentValue, ledgerValue) {
  const document = normalizeCreationDocument(documentValue, { compatibilityAliases: false });
  const properties = document.metadata.properties || {};
  return properties.groundingVerified === true
    && properties.groundingStatus === 'verified'
    && isRecord(ledgerValue)
    && ledgerValue.status === 'verified';
}
