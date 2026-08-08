import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, rm } from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import test, { after, before } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import {
  cleanupIncompleteRelease,
  verifyPrepare,
  verifyPrepublish,
} from './verify-release-version.mjs';

const execFileAsync = promisify(execFile);
const sourceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixtureFiles = [
  'package.json',
  'package-lock.json',
  'CHANGELOG.md',
  'README.md',
  'SECURITY.md',
  'desktop-ui/index.html',
  'desktop-ui/app.js',
  'resources/creation/catalog/runtime-bundle.json',
  'scripts/generate-creation-resources.mjs',
  'skills/video-content-analysis/scripts/yunspire_speech_info.plist',
  'src-tauri/tauri.conf.json',
  'src-tauri/Cargo.toml',
  'src-tauri/Cargo.lock',
  'docs/AI_ASSISTANT_INSTRUCTIONS.md',
  'docs/BRAND_GUIDE.md',
  'docs/MEMORY_V2.md',
  'docs/PRODUCT_REQUIREMENTS.md',
  'docs/RELEASING.md',
  'docs/schemas/README.md',
  'docs/assets/architecture-overview.svg',
  '.github/ISSUE_TEMPLATE/bug_report.yml',
];
const repository = 'example/yunspire';
const releaseTag = 'v0.3.0';
const tagObjectSha = 'a'.repeat(40);
let fixtureRoot;
let fixtureCommit;

async function git(argumentsList) {
  const { stdout } = await execFileAsync('git', argumentsList, {
    cwd: fixtureRoot,
    encoding: 'utf8',
    windowsHide: true,
  });
  return stdout.trim();
}

before(async () => {
  fixtureRoot = await mkdtemp(path.join(os.tmpdir(), 'yunspire-release-contract-'));
  for (const relativePath of fixtureFiles) {
    const destination = path.join(fixtureRoot, relativePath);
    await mkdir(path.dirname(destination), { recursive: true });
    await copyFile(path.join(sourceRoot, relativePath), destination);
  }
  await git(['init', '--quiet']);
  await git(['config', 'user.name', 'Yunspire Release Contract']);
  await git(['config', 'user.email', 'release-contract@example.invalid']);
  await git(['config', 'commit.gpgsign', 'false']);
  await git(['config', 'core.autocrlf', 'false']);
  await git(['add', '--all']);
  await git(['commit', '--quiet', '-m', 'release contract fixture']);
  fixtureCommit = await git(['rev-parse', 'HEAD']);
  assert.match(fixtureCommit, /^[0-9a-f]{40}$/u);
  assert.equal(await git(['status', '--porcelain=v1', '--untracked-files=all']), '');
});

after(async () => {
  if (fixtureRoot) await rm(fixtureRoot, { recursive: true, force: true });
});

function fillerReleases(count, startId = 1) {
  return Array.from({ length: count }, (_, index) => ({
    id: startId + index,
    tag_name: `v0.0.${startId + index}`,
  }));
}

function ownedDraft(id, provenance) {
  return {
    id,
    tag_name: releaseTag,
    draft: true,
    prerelease: false,
    target_commitish: fixtureCommit,
    body: `# Yunspire 0.3.0\n\n<!-- ${provenance} -->`,
  };
}

async function withMockGitHub({ pages, provenance = null, tagPresent = false }, callback) {
  const requests = [];
  const server = http.createServer((request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1');
    requests.push(`${request.method} ${url.pathname}${url.search}`);
    const respond = (status, value) => {
      response.writeHead(status, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify(value));
    };

    if (request.method !== 'GET') return respond(405, { message: 'method not allowed' });
    if (url.pathname === `/repos/${repository}/immutable-releases`) {
      return respond(200, { enabled: true, enforced_by_owner: false });
    }
    if (url.pathname === `/repos/${repository}/git/ref/heads/main`) {
      return respond(200, { object: { type: 'commit', sha: fixtureCommit } });
    }
    if (url.pathname === `/repos/${repository}/git/ref/tags/${releaseTag}`) {
      return tagPresent
        ? respond(200, { object: { type: 'tag', sha: tagObjectSha } })
        : respond(404, { message: 'not found' });
    }
    if (url.pathname === `/repos/${repository}/git/tags/${tagObjectSha}`) {
      return respond(200, {
        tag: releaseTag,
        message: `Yunspire 0.3.0 release\n${provenance}`,
        object: { type: 'commit', sha: fixtureCommit },
      });
    }
    if (url.pathname === `/repos/${repository}/releases`) {
      const page = Number(url.searchParams.get('page') || '1');
      return respond(200, pages.get(page) || []);
    }
    if (url.pathname === `/repos/${repository}/releases/tags/${releaseTag}`) {
      return respond(404, { message: 'drafts are not visible through this endpoint' });
    }
    return respond(404, { message: 'unexpected endpoint' });
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address === 'object');
  const previousApiRoot = process.env.GITHUB_API_URL;
  const previousToken = process.env.GH_TOKEN;
  process.env.GITHUB_API_URL = `http://127.0.0.1:${address.port}`;
  process.env.GH_TOKEN = 'release-contract-token';
  try {
    return await callback(requests);
  } finally {
    if (previousApiRoot === undefined) delete process.env.GITHUB_API_URL;
    else process.env.GITHUB_API_URL = previousApiRoot;
    if (previousToken === undefined) delete process.env.GH_TOKEN;
    else process.env.GH_TOKEN = previousToken;
    await new Promise((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
  }
}

async function withCleanupMock({
  release,
  provenance,
  tagPresent = true,
  immutableEnabled = true,
}, callback) {
  const requests = [];
  let releaseState = release ? structuredClone(release) : null;
  let tagState = tagPresent;
  const server = http.createServer((request, response) => {
    const url = new URL(request.url, 'http://127.0.0.1');
    requests.push(`${request.method} ${url.pathname}${url.search}`);
    const respond = (status, value = null) => {
      if (status === 204) {
        response.writeHead(status);
        response.end();
        return;
      }
      response.writeHead(status, { 'Content-Type': 'application/json' });
      response.end(JSON.stringify(value));
    };

    if (request.method === 'GET' && url.pathname === `/repos/${repository}/immutable-releases`) {
      return respond(200, { enabled: immutableEnabled, enforced_by_owner: false });
    }
    if (request.method === 'GET' && url.pathname === `/repos/${repository}/releases`) {
      return respond(200, releaseState ? [releaseState] : []);
    }
    if (url.pathname.match(new RegExp(`^/repos/${repository}/releases/[0-9]+$`, 'u'))) {
      const releaseId = Number(url.pathname.split('/').at(-1));
      if (request.method === 'GET') {
        return releaseState?.id === releaseId
          ? respond(200, releaseState)
          : respond(404, { message: 'not found' });
      }
      if (request.method === 'DELETE') {
        if (releaseState?.id !== releaseId) return respond(404, { message: 'not found' });
        releaseState = null;
        return respond(204);
      }
    }
    if (url.pathname === `/repos/${repository}/git/ref/tags/${releaseTag}` && request.method === 'GET') {
      return tagState
        ? respond(200, { object: { type: 'tag', sha: tagObjectSha } })
        : respond(404, { message: 'not found' });
    }
    if (url.pathname === `/repos/${repository}/git/tags/${tagObjectSha}` && request.method === 'GET') {
      return respond(200, {
        tag: releaseTag,
        message: `Yunspire 0.3.0 release\n${provenance}`,
        object: { type: 'commit', sha: fixtureCommit },
      });
    }
    if (url.pathname === `/repos/${repository}/git/refs/tags/${releaseTag}` && request.method === 'DELETE') {
      if (!tagState) return respond(404, { message: 'not found' });
      tagState = false;
      return respond(204);
    }
    return respond(404, { message: 'unexpected endpoint' });
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address === 'object');
  const previousApiRoot = process.env.GITHUB_API_URL;
  const previousToken = process.env.GH_TOKEN;
  process.env.GITHUB_API_URL = `http://127.0.0.1:${address.port}`;
  process.env.GH_TOKEN = 'release-contract-token';
  try {
    return await callback(requests, () => ({ release: releaseState, tagPresent: tagState }));
  } finally {
    if (previousApiRoot === undefined) delete process.env.GITHUB_API_URL;
    else process.env.GITHUB_API_URL = previousApiRoot;
    if (previousToken === undefined) delete process.env.GH_TOKEN;
    else process.env.GH_TOKEN = previousToken;
    await new Promise((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
  }
}

test('prepare scans past a full first page before declaring the Release absent', async () => {
  await withMockGitHub({
    pages: new Map([
      [1, fillerReleases(100)],
      [2, []],
    ]),
  }, async (requests) => {
    const identity = await verifyPrepare({
      repository,
      releaseBranch: `release/${releaseTag}`,
      expectedCommit: fixtureCommit,
      root: fixtureRoot,
    });
    assert.equal(identity.sourceCommit, fixtureCommit);
    assert.ok(requests.some((request) => request.includes('/releases?per_page=100&page=2')));
    assert.equal(requests.some((request) => request.includes('/releases/tags/')), false);
  });
});

test('prepare rejects a draft found only on a later page', async () => {
  const provenance = `yunspire-release-run:10:1:${fixtureCommit}`;
  await withMockGitHub({
    pages: new Map([
      [1, fillerReleases(100)],
      [2, [ownedDraft(2_001, provenance)]],
    ]),
  }, async () => {
    await assert.rejects(
      verifyPrepare({
        repository,
        releaseBranch: `release/${releaseTag}`,
        expectedCommit: fixtureCommit,
        root: fixtureRoot,
      }),
      /already exists/u,
    );
  });
});

test('prepublish fails closed when the same tag resolves to two Release IDs across pages', async () => {
  const provenance = `yunspire-release-run:11:2:${fixtureCommit}`;
  await withMockGitHub({
    tagPresent: true,
    provenance,
    pages: new Map([
      [1, [ownedDraft(3_001, provenance), ...fillerReleases(99, 10_000)]],
      [2, [ownedDraft(3_002, provenance)]],
    ]),
  }, async () => {
    await assert.rejects(
      verifyPrepublish({
        repository,
        tag: releaseTag,
        expectedCommit: fixtureCommit,
        expectedProvenance: provenance,
        root: fixtureRoot,
      }),
      /exactly one draft; found 2/u,
    );
  });
});

test('prepublish finds and locks the unique draft Release on a later page', async () => {
  const provenance = `yunspire-release-run:12:3:${fixtureCommit}`;
  await withMockGitHub({
    tagPresent: true,
    provenance,
    pages: new Map([
      [1, fillerReleases(100, 20_000)],
      [2, [ownedDraft(4_001, provenance)]],
    ]),
  }, async (requests) => {
    const identity = await verifyPrepublish({
      repository,
      tag: releaseTag,
      expectedCommit: fixtureCommit,
      expectedProvenance: provenance,
      root: fixtureRoot,
    });
    assert.equal(identity.releaseId, 4_001);
    assert.equal(identity.tagObjectSha, tagObjectSha);
    assert.equal(requests.some((request) => request.includes('/releases/tags/')), false);
  });
});

test('failure recovery validates and preserves the locked owned draft and annotated tag', async () => {
  const provenance = `yunspire-release-run:13:1:${fixtureCommit}`;
  const releaseId = 5_001;
  await withCleanupMock({
    release: ownedDraft(releaseId, provenance),
    provenance,
  }, async (requests, state) => {
    const result = await cleanupIncompleteRelease({
      repository,
      tag: releaseTag,
      expectedCommit: fixtureCommit,
      expectedProvenance: provenance,
      expectedReleaseId: releaseId,
    });
    assert.equal(result.action, 'preserved-draft-and-tag');
    assert.equal(result.releaseId, releaseId);
    assert.equal(result.deletedTag, false);
    assert.equal(result.tagPreserved, true);
    assert.deepEqual(state(), { release: ownedDraft(releaseId, provenance), tagPresent: true });
    assert.equal(requests.includes(`DELETE /repos/${repository}/releases/${releaseId}`), false);
    assert.equal(requests.includes(`DELETE /repos/${repository}/git/refs/tags/${releaseTag}`), false);
  });
});

test('cleanup protects a published Release even when its ID is supplied', async () => {
  const provenance = `yunspire-release-run:14:1:${fixtureCommit}`;
  const release = { ...ownedDraft(5_002, provenance), draft: false };
  await withCleanupMock({ release, provenance }, async (requests, state) => {
    const result = await cleanupIncompleteRelease({
      repository,
      tag: releaseTag,
      expectedCommit: fixtureCommit,
      expectedProvenance: provenance,
      expectedReleaseId: release.id,
    });
    assert.equal(result.action, 'preserved-published');
    assert.deepEqual(state(), { release, tagPresent: true });
    assert.equal(requests.some((request) => request.startsWith('DELETE ')), false);
  });
});

test('cleanup refuses ambiguous same-tag Releases before any deletion', async () => {
  const provenance = `yunspire-release-run:15:1:${fixtureCommit}`;
  await withMockGitHub({
    provenance,
    pages: new Map([[1, [ownedDraft(5_003, provenance), ownedDraft(5_004, provenance)]]]),
  }, async (requests) => {
    await assert.rejects(
      cleanupIncompleteRelease({
        repository,
        tag: releaseTag,
        expectedCommit: fixtureCommit,
        expectedProvenance: provenance,
      }),
      /refusing ambiguous deletion/u,
    );
    assert.equal(requests.some((request) => request.startsWith('DELETE ')), false);
  });
});

test('failure recovery stays non-destructive without repository administration access', async () => {
  const provenance = `yunspire-release-run:16:1:${fixtureCommit}`;
  const release = ownedDraft(5_005, provenance);
  await withCleanupMock({
    release,
    provenance,
    immutableEnabled: false,
  }, async (requests, state) => {
    const result = await cleanupIncompleteRelease({
      repository,
      tag: releaseTag,
      expectedCommit: fixtureCommit,
      expectedProvenance: provenance,
      expectedReleaseId: release.id,
    });
    assert.equal(result.action, 'preserved-draft-and-tag');
    assert.deepEqual(state(), { release, tagPresent: true });
    assert.equal(requests.some((request) => request.startsWith('DELETE ')), false);
    assert.equal(requests.some((request) => request.includes('/immutable-releases')), false);
  });
});
