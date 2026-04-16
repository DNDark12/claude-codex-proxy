import test from 'node:test';
import assert from 'node:assert/strict';
import { lstat, mkdtemp, readFile, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

import {
  addSkill,
  doctorProject,
  isSymbolicLink,
  listInstalledSkills,
  removeSkills,
  updateSkills,
} from '../src/install.mjs';

const repoRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../..');
const fixtureSkillDir = path.join(repoRoot, 'skills', 'code-review');

test('add installs a project-local skill with symlinks and aggregated registry', async () => {
  const projectRoot = await mkdtemp(path.join(os.tmpdir(), 'skills-cli-project-'));

  try {
    const result = await addSkill({
      source: fixtureSkillDir,
      projectRoot,
      agents: ['codex', 'claude-code'],
      installMode: 'symlink',
    });

    assert.equal(result.id, 'code-review');

    const registry = JSON.parse(
      await readFile(path.join(projectRoot, '.bridge', 'registry.json'), 'utf8'),
    );
    assert.equal(registry.skills.length, 1);
    assert.equal(
      registry.skills[0].codex_artifact_path,
      'store/code-review/1.0.0/dist/code-review/SKILL.md',
    );
    assert.equal(
      registry.skills[0].reference_bundle_path,
      'store/code-review/1.0.0/dist/code-review/references.json',
    );

    assert.equal(
      await isSymbolicLink(path.join(projectRoot, '.bridge', 'agents', 'codex', 'code-review')),
      true,
    );
    assert.equal(
      await isSymbolicLink(
        path.join(projectRoot, '.bridge', 'agents', 'claude-code', 'code-review'),
      ),
      true,
    );
    assert.match(result.activationHints.claudeCode, /claude --plugin-dir/);
  } finally {
    await rm(projectRoot, { recursive: true, force: true });
  }
});

test('add supports copy mode and list returns installed skill metadata', async () => {
  const projectRoot = await mkdtemp(path.join(os.tmpdir(), 'skills-cli-copy-project-'));

  try {
    await addSkill({
      source: fixtureSkillDir,
      projectRoot,
      agents: ['codex'],
      installMode: 'copy',
    });

    const installed = await listInstalledSkills(projectRoot);
    assert.equal(installed.length, 1);
    assert.deepEqual(installed[0].agents, ['codex']);
    assert.equal(installed[0].installMode, 'copy');

    const stat = await lstat(path.join(projectRoot, '.bridge', 'agents', 'codex', 'code-review'));
    assert.equal(stat.isSymbolicLink(), false);
  } finally {
    await rm(projectRoot, { recursive: true, force: true });
  }
});

test('remove deletes store and clears aggregated registry entry', async () => {
  const projectRoot = await mkdtemp(path.join(os.tmpdir(), 'skills-cli-remove-project-'));

  try {
    await addSkill({
      source: fixtureSkillDir,
      projectRoot,
      agents: ['codex'],
      installMode: 'symlink',
    });

    const removed = await removeSkills({
      projectRoot,
      skillIds: ['code-review'],
    });
    assert.equal(removed.length, 1);

    const installed = await listInstalledSkills(projectRoot);
    assert.equal(installed.length, 0);

    const registry = JSON.parse(
      await readFile(path.join(projectRoot, '.bridge', 'registry.json'), 'utf8'),
    );
    assert.deepEqual(registry.skills, []);
  } finally {
    await rm(projectRoot, { recursive: true, force: true });
  }
});

test('update reinstalls a missing agent view and doctor reports healthy afterwards', async () => {
  const projectRoot = await mkdtemp(path.join(os.tmpdir(), 'skills-cli-update-project-'));

  try {
    await addSkill({
      source: fixtureSkillDir,
      projectRoot,
      agents: ['codex'],
      installMode: 'symlink',
    });

    const targetPath = path.join(projectRoot, '.bridge', 'agents', 'codex', 'code-review');
    await rm(targetPath, { recursive: true, force: true });

    const unhealthy = await doctorProject(projectRoot);
    assert.equal(unhealthy.healthy, false);
    assert.match(unhealthy.issues[0].code, /missing_agent_view/);

    const updated = await updateSkills({
      projectRoot,
      skillIds: ['code-review'],
    });
    assert.equal(updated.length, 1);

    const healthy = await doctorProject(projectRoot);
    assert.equal(healthy.healthy, true);
  } finally {
    await rm(projectRoot, { recursive: true, force: true });
  }
});

test('claude-code install target contains a valid plugin manifest and command bundle', async () => {
  const projectRoot = await mkdtemp(path.join(os.tmpdir(), 'skills-cli-claude-project-'));

  try {
    await addSkill({
      source: fixtureSkillDir,
      projectRoot,
      agents: ['claude-code'],
      installMode: 'copy',
    });

    const pluginRoot = path.join(projectRoot, '.bridge', 'agents', 'claude-code', 'code-review');
    const pluginManifest = JSON.parse(
      await readFile(path.join(pluginRoot, '.claude-plugin', 'plugin.json'), 'utf8'),
    );
    const commandBody = await readFile(
      path.join(pluginRoot, 'commands', 'code-review.md'),
      'utf8',
    );

    assert.equal(pluginManifest.name, 'code-review');
    assert.match(commandBody, /skill-bridge:code-review@1.0.0/);

    const report = await doctorProject(projectRoot);
    assert.equal(report.healthy, true);
  } finally {
    await rm(projectRoot, { recursive: true, force: true });
  }
});
