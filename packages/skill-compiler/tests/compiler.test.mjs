import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile, mkdir } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

import { compileSkill } from '../src/compiler.mjs';
import { loadSkillManifest } from '../src/manifest.mjs';

const repoRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../..');
const fixtureSkillDir = path.join(repoRoot, 'skills', 'code-review');
const runtimeFixtureDir = path.join(repoRoot, 'tests', 'fixtures', 'skill_bridge');

test('manifest validation fails fast on missing required field', async () => {
  const tempDir = await mkdtemp(path.join(os.tmpdir(), 'skill-compiler-invalid-'));

  try {
    await writeFile(
      path.join(tempDir, 'skill.yaml'),
      JSON.stringify({
        schema_version: '1',
        id: 'broken-skill',
      }),
      'utf8',
    );

    await assert.rejects(
      () => loadSkillManifest(tempDir),
      /Invalid manifest\.version: expected non-empty string/,
    );
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
});

test('compiler emits deterministic codex and registry artifacts for code-review', async () => {
  const outDir = await mkdtemp(path.join(os.tmpdir(), 'skill-compiler-out-'));

  try {
    const result = await compileSkill({
      skillDir: fixtureSkillDir,
      outDir,
    });

    const compiledSkill = await readFile(result.codexSkillPath, 'utf8');
    const expectedSkill = await readFile(
      path.join(runtimeFixtureDir, 'code-review', 'SKILL.md'),
      'utf8',
    );
    assert.equal(compiledSkill, expectedSkill);

    const compiledRegistry = JSON.parse(await readFile(result.registryPath, 'utf8'));
    const expectedRegistry = JSON.parse(
      await readFile(path.join(runtimeFixtureDir, 'registry.json'), 'utf8'),
    );
    assert.deepEqual(compiledRegistry, expectedRegistry);

    const compiledReferences = JSON.parse(await readFile(result.referenceBundlePath, 'utf8'));
    const expectedReferences = JSON.parse(
      await readFile(path.join(runtimeFixtureDir, 'code-review', 'references.json'), 'utf8'),
    );
    assert.deepEqual(compiledReferences, expectedReferences);

    const compiledPlugin = JSON.parse(await readFile(result.claudePluginPath, 'utf8'));
    assert.deepEqual(compiledPlugin, {
      name: 'code-review',
      version: '1.0.0',
      description: 'Review repository changes for correctness and risk.',
      author: {
        name: 'codex-openai-proxy',
      },
    });

    const compiledCommand = await readFile(result.claudeCommandPath, 'utf8');
    assert.match(compiledCommand, /skill-bridge:code-review@1.0.0/);
    assert.match(compiledCommand, /# Code Review/);
  } finally {
    await rm(outDir, { recursive: true, force: true });
  }
});

test('compiler validate path loads the canonical skill source', async () => {
  const manifest = await loadSkillManifest(fixtureSkillDir);
  assert.equal(manifest.id, 'code-review');
  assert.equal(manifest.mapping.mergeMode, 'prepend');
  assert.deepEqual(manifest.references, ['references/review-rubric.md']);
});
