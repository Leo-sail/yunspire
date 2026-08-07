function streamFailure(code, message, details = {}) {
  const error = new Error(message);
  error.code = code;
  error.details = { ...details };
  return error;
}

function providerSequence(value) {
  const sequence = Number(value);
  if (!Number.isSafeInteger(sequence) || sequence < 0) {
    throw streamFailure(
      'creation.model-stream.invalid-sequence',
      '模型正文增量缺少有效的 providerSequence',
      { received: value },
    );
  }
  return sequence;
}

export function createCreationModelStreamBatch({ batchIndex = 0 } = {}) {
  const index = Number(batchIndex);
  if (!Number.isSafeInteger(index) || index < 0) throw new TypeError('Creation model stream batchIndex must be a non-negative integer');

  let expectedSequence = 0;
  let receivedContent = '';
  let receivedDeltaCount = 0;
  let failure = null;

  function reject(error) {
    failure ||= error;
    return { accepted: false, ignored: false, error: failure };
  }

  function accept(payloadValue) {
    const payload = payloadValue && typeof payloadValue === 'object' ? payloadValue : {};
    if (payload.kind !== 'contentDelta') return { accepted: false, ignored: true, error: null };
    if (failure) return { accepted: false, ignored: false, error: failure };

    let sequence;
    try {
      sequence = providerSequence(payload.providerSequence);
    } catch (error) {
      return reject(error);
    }
    if (sequence < expectedSequence) {
      return reject(streamFailure(
        'creation.model-stream.replayed-sequence',
        `模型正文增量序号重复或倒序：期望 ${expectedSequence}，实际 ${sequence}`,
        { expectedSequence, receivedSequence: sequence },
      ));
    }
    if (sequence > expectedSequence) {
      return reject(streamFailure(
        'creation.model-stream.sequence-gap',
        `模型正文增量序号存在缺口：期望 ${expectedSequence}，实际 ${sequence}`,
        { expectedSequence, receivedSequence: sequence },
      ));
    }
    if (payload.channel != null && payload.channel !== 'text') {
      return reject(streamFailure(
        'creation.model-stream.invalid-channel',
        `模型正文增量通道无效：${String(payload.channel)}`,
        { channel: payload.channel },
      ));
    }
    if (typeof payload.contentDelta !== 'string' || payload.contentDelta.length === 0) {
      return reject(streamFailure(
        'creation.model-stream.empty-delta',
        '模型正文增量为空，无法作为真实 Creation Agent Stream 内容',
        { providerSequence: sequence },
      ));
    }

    const prefix = index > 0 && receivedDeltaCount === 0 ? '\n\n' : '';
    receivedContent += payload.contentDelta;
    receivedDeltaCount += 1;
    expectedSequence += 1;
    return {
      accepted: true,
      ignored: false,
      error: null,
      providerSequence: sequence,
      channel: 'text',
      content: `${prefix}${payload.contentDelta}`,
    };
  }

  function verify(finalMarkdown) {
    if (failure) throw failure;
    if (receivedDeltaCount === 0) {
      throw streamFailure(
        'creation.model-stream.missing-delta',
        '模型没有提供真实正文增量，已拒绝用最终完整结果伪造 Creation Agent Stream',
      );
    }
    const finalContent = String(finalMarkdown ?? '');
    if (receivedContent !== finalContent) {
      throw streamFailure(
        'creation.model-stream.receipt-mismatch',
        '模型流式正文与最终回执不一致，已阻止生成候选',
        {
          streamedLength: receivedContent.length,
          receiptLength: finalContent.length,
        },
      );
    }
    return receivedContent;
  }

  function snapshot() {
    return {
      batchIndex: index,
      expectedSequence,
      receivedContent,
      receivedDeltaCount,
      failure,
    };
  }

  return Object.freeze({ accept, snapshot, verify });
}
