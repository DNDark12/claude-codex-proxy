import { cp, lstat, mkdir, readFile, rm, symlink } from 'node:fs/promises';
import path from 'node:path';

import { compileSkill } from '../../skill-compiler/src/compiler.mjs';
import { loadSkillManifest } from '../../skill-compiler/src/manifest.mjs';
import {
  BRIDGE_DIRNAME,
  getBridgeRoot,
  getRegistryPath,
  loadInstallState,
  saveInstallState,
  writeAggregatedRegistry,
} from './state.mjs';

const SUPPORTED_AGENTS = new Set(['claude-code', 'codex']);

export async function addSkill({
  source,
  projectRoot,
  agents = ['claude-code', 'codex'],
  installMode = 'symlink',
}) {
  if (!SUPPORTED_AGENTS.has('claude-code') || !SUPPORTED_AGENTS.has('codex')) {
    throw new Error('skills-cli misconfiguration: supported agents set is invalid');
  }
  const selectedAgents = normalizeAgents(agents);
  if (installMode !== 'symlink' && installMode !== 'copy') {
    throw new Error(`Unsupported install mode "${installMode}"`);
  }

  const sourcePath = path.resolve(projectRoot, source);
  const manifest = await loadSkillManifest(sourcePath);
  const bridgeRoot = getBridgeRoot(projectRoot);
  const storeRoot = path.join(bridgeRoot, 'store', manifest.id, manifest.version);
  const canonicalDir = path.join(storeRoot, 'canonical');
  const distDir = path.join(storeRoot, 'dist');

  await rm(storeRoot, { recursive: true, force: true });
  await mkdir(path.dirname(storeRoot), { recursive: true });
  await cp(sourcePath, canonicalDir, { recursive: true });

  const compiled = await compileSkill({
    skillDir: sourcePath,
    outDir: distDir,
  });

  for (const agent of selectedAgents) {
    await installAgentView({
      agent,
      projectRoot,
      skillId: manifest.id,
      distDir,
      installMode,
    });
  }

  const registryEntry = {
    id: manifest.id,
    version: manifest.version,
    marker: manifest.activation.marker,
    codex_artifact_path: path
      .relative(bridgeRoot, compiled.codexSkillPath)
      .replaceAll(path.sep, '/'),
    reference_bundle_path: path
      .relative(bridgeRoot, compiled.referenceBundlePath)
      .replaceAll(path.sep, '/'),
    merge_mode: manifest.mapping.mergeMode,
    tool_aliases: manifest.toolAliases,
    compatibility: {
      anthropic: manifest.compatibility.claudeCode,
      codex: manifest.compatibility.codex,
    },
  };

  const state = await loadInstallState(projectRoot);
  const storedSourcePath = normalizeStoredSourcePath(projectRoot, sourcePath);
  const skillRecord = {
    id: manifest.id,
    version: manifest.version,
    source: storedSourcePath,
    agents: selectedAgents,
    installMode,
    storeDir: path.relative(projectRoot, storeRoot).replaceAll(path.sep, '/'),
    activationHints: buildActivationHints(projectRoot, manifest.id, selectedAgents),
    registryEntry,
  };

  state.skills = state.skills
    .filter((entry) => !(entry.id === manifest.id && entry.version === manifest.version))
    .concat(skillRecord)
    .sort((left, right) => `${left.id}@${left.version}`.localeCompare(`${right.id}@${right.version}`));

  await saveInstallState(projectRoot, state);
  await writeAggregatedRegistry(
    projectRoot,
    state.skills.map((entry) => entry.registryEntry),
  );

  return skillRecord;
}

export async function listInstalledSkills(projectRoot) {
  const state = await loadInstallState(projectRoot);
  return state.skills;
}

export async function removeSkills({ projectRoot, skillIds }) {
  if (!Array.isArray(skillIds) || skillIds.length === 0) {
    throw new Error('removeSkills requires at least one skill id');
  }

  const state = await loadInstallState(projectRoot);
  const targetIds = new Set(skillIds.map((value) => value.trim().toLowerCase()));
  const removed = state.skills.filter((entry) => targetIds.has(entry.id.toLowerCase()));
  const kept = state.skills.filter((entry) => !targetIds.has(entry.id.toLowerCase()));

  for (const entry of removed) {
    await rm(path.join(projectRoot, entry.storeDir), { recursive: true, force: true });
    for (const agent of entry.agents) {
      await rm(path.join(getBridgeRoot(projectRoot), 'agents', agent, entry.id), {
        recursive: true,
        force: true,
      });
    }
  }

  state.skills = kept;
  await saveInstallState(projectRoot, state);
  await writeAggregatedRegistry(
    projectRoot,
    state.skills.map((entry) => entry.registryEntry),
  );

  return removed;
}

export async function updateSkills({ projectRoot, skillIds = [] }) {
  const state = await loadInstallState(projectRoot);
  const selected =
    skillIds.length === 0
      ? state.skills
      : state.skills.filter((entry) =>
          new Set(skillIds.map((value) => value.trim().toLowerCase())).has(entry.id.toLowerCase()),
        );

  const updated = [];
  for (const entry of selected) {
    updated.push(
      await addSkill({
        source: entry.source,
        projectRoot,
        agents: entry.agents,
        installMode: entry.installMode,
      }),
    );
  }

  return updated;
}

export async function doctorProject(projectRoot) {
  const state = await loadInstallState(projectRoot);
  const registryPath = getRegistryPath(projectRoot);
  const issues = [];
  let registry = null;

  try {
    registry = JSON.parse(await readFile(registryPath, 'utf8'));
  } catch (error) {
    issues.push({
      code: 'missing_registry',
      message: `Missing or unreadable registry at ${registryPath}: ${error.message}`,
    });
  }

  for (const entry of state.skills) {
    const storePath = path.join(projectRoot, entry.storeDir);
    const storeExists = await pathExists(storePath);
    if (!storeExists) {
      issues.push({
        code: 'missing_store',
        message: `Missing store directory for ${entry.id}@${entry.version}: ${storePath}`,
      });
    }

    const registryMatch = registry?.skills?.find(
      (candidate) => candidate.id === entry.id && candidate.version === entry.version,
    );
    if (!registryMatch) {
      issues.push({
        code: 'missing_registry_entry',
        message: `Missing aggregated registry entry for ${entry.id}@${entry.version}`,
      });
    }
    if (registryMatch?.reference_bundle_path) {
      const referenceBundlePath = path.join(projectRoot, BRIDGE_DIRNAME, registryMatch.reference_bundle_path);
      if (!(await pathExists(referenceBundlePath))) {
        issues.push({
          code: 'missing_reference_bundle',
          message: `Missing reference bundle for ${entry.id}@${entry.version}: ${referenceBundlePath}`,
        });
      }
    }

    for (const agent of entry.agents) {
      const agentPath = path.join(getBridgeRoot(projectRoot), 'agents', agent, entry.id);
      const exists = await pathExists(agentPath);
      if (!exists) {
        issues.push({
          code: 'missing_agent_view',
          message: `Missing ${agent} install target for ${entry.id}@${entry.version}: ${agentPath}`,
        });
        continue;
      }

      const symlink = await isSymbolicLink(agentPath);
      if (entry.installMode === 'symlink' && !symlink) {
        issues.push({
          code: 'expected_symlink',
          message: `Expected symlink install for ${agent}:${entry.id} at ${agentPath}`,
        });
      }
      if (entry.installMode === 'copy' && symlink) {
        issues.push({
          code: 'unexpected_symlink',
          message: `Expected copied install for ${agent}:${entry.id} at ${agentPath}`,
        });
      }

      if (agent === 'claude-code') {
        const pluginManifestPath = path.join(agentPath, '.claude-plugin', 'plugin.json');
        const commandsPath = path.join(agentPath, 'commands');
        if (!(await pathExists(pluginManifestPath))) {
          issues.push({
            code: 'missing_claude_plugin_manifest',
            message: `Missing Claude plugin manifest for ${entry.id}@${entry.version}: ${pluginManifestPath}`,
          });
        }
        if (!(await pathExists(commandsPath))) {
          issues.push({
            code: 'missing_claude_plugin_commands',
            message: `Missing Claude plugin commands for ${entry.id}@${entry.version}: ${commandsPath}`,
          });
        }
      }
    }
  }

  return {
    healthy: issues.length === 0,
    issues,
    installedSkills: state.skills.length,
  };
}

async function installAgentView({ agent, projectRoot, skillId, distDir, installMode }) {
  const sourceDir =
    agent === 'codex'
      ? path.join(distDir, skillId)
      : path.join(distDir, 'claude', skillId);
  const targetDir = path.join(getBridgeRoot(projectRoot), 'agents', agent, skillId);

  await rm(targetDir, { recursive: true, force: true });
  await mkdir(path.dirname(targetDir), { recursive: true });

  if (installMode === 'copy') {
    await cp(sourceDir, targetDir, { recursive: true });
    return;
  }

  const relativeSource = path.relative(path.dirname(targetDir), sourceDir) || '.';
  await symlink(relativeSource, targetDir, 'dir');
}

function normalizeAgents(agents) {
  const normalized = (agents.length === 0 ? ['claude-code', 'codex'] : agents).map((value) =>
    value.trim().toLowerCase(),
  );
  for (const agent of normalized) {
    if (!SUPPORTED_AGENTS.has(agent)) {
      throw new Error(`Unsupported agent "${agent}"`);
    }
  }
  return [...new Set(normalized)].sort();
}

function normalizeStoredSourcePath(projectRoot, sourcePath) {
  const relativePath = path.relative(projectRoot, sourcePath);
  if (
    relativePath &&
    !relativePath.startsWith('..') &&
    !path.isAbsolute(relativePath)
  ) {
    return relativePath.replaceAll(path.sep, '/');
  }
  return sourcePath;
}

function buildActivationHints(projectRoot, skillId, agents) {
  const hints = {};
  if (agents.includes('claude-code')) {
    const pluginDir = path.join(getBridgeRoot(projectRoot), 'agents', 'claude-code', skillId);
    hints.claudeCode = `claude --plugin-dir ${pluginDir}`;
  }
  if (agents.includes('codex')) {
    const skillDir = path.join(getBridgeRoot(projectRoot), 'agents', 'codex', skillId);
    hints.codex = `PROXY_SKILLS_REGISTRY_PATH=${path.join(getBridgeRoot(projectRoot), 'registry.json')} codex`;
    hints.codexSkillDir = skillDir;
  }
  return hints;
}

export async function isSymbolicLink(pathname) {
  try {
    const stat = await lstat(pathname);
    return stat.isSymbolicLink();
  } catch (error) {
    if (error.code === 'ENOENT') {
      return false;
    }
    throw error;
  }
}

async function pathExists(pathname) {
  try {
    await lstat(pathname);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT') {
      return false;
    }
    throw error;
  }
}
