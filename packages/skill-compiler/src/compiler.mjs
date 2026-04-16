import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

import { loadSkillManifest } from './manifest.mjs';

export async function compileSkill({ skillDir, outDir }) {
  const manifest = await loadSkillManifest(skillDir);
  const claudePrompt = await loadClaudePrompt(skillDir);
  const referenceBundle = await loadReferenceBundle(skillDir, manifest.references);

  const codexSkillPath = path.join(outDir, manifest.artifacts.codexEntry);
  const claudePluginRoot = path.join(outDir, manifest.artifacts.claudeEntry);
  const claudePluginPath = path.join(claudePluginRoot, '.claude-plugin', 'plugin.json');
  const claudeCommandPath = path.join(claudePluginRoot, 'commands', `${manifest.id}.md`);
  const referenceBundlePath = path.join(outDir, manifest.mapping.codexSkill, 'references.json');
  const registryPath = path.join(outDir, 'registry.json');

  const codexMarkdown = generateCodexSkillMarkdown(manifest, claudePrompt);
  const pluginJson = generateClaudePluginJson(manifest);
  const commandMarkdown = generateClaudeCommandMarkdown(manifest, claudePrompt);
  const registry = generateRegistry(manifest);

  await writeTextFile(codexSkillPath, codexMarkdown);
  await writeTextFile(claudePluginPath, JSON.stringify(pluginJson, null, 2) + '\n');
  await writeTextFile(claudeCommandPath, commandMarkdown);
  await writeTextFile(
    referenceBundlePath,
    JSON.stringify({ references: referenceBundle }, null, 2) + '\n',
  );
  await writeTextFile(
    registryPath,
    JSON.stringify(registry, null, 2) + '\n',
  );

  return {
    manifest,
    codexSkillPath,
    claudePluginPath,
    claudeCommandPath,
    referenceBundlePath,
    registryPath,
  };
}

export function generateCodexSkillMarkdown(manifest, claudePrompt) {
  return `---\nname: ${manifest.id}\ndescription: ${manifest.description}\n---\n\n${claudePrompt.trim()}\n`;
}

export function generateClaudePluginJson(manifest) {
  return {
    name: manifest.id,
    version: manifest.version,
    description: manifest.description,
    author: {
      name: 'codex-openai-proxy',
    },
  };
}

export function generateClaudeCommandMarkdown(manifest, claudePrompt) {
  return `---\ndescription: Activate ${manifest.displayName} through the skill bridge.\n---\n\n${manifest.activation.marker}\n\n${claudePrompt.trim()}\n`;
}

export function generateRegistry(manifest) {
  return {
    version: '1',
    skills: [
      {
        id: manifest.id,
        version: manifest.version,
        marker: manifest.activation.marker,
        codex_artifact_path: manifest.artifacts.codexEntry.replaceAll(path.sep, '/'),
        reference_bundle_path:
          manifest.references.length === 0
            ? null
            : `${manifest.mapping.codexSkill}/references.json`,
        merge_mode: manifest.mapping.mergeMode,
        tool_aliases: manifest.toolAliases,
        compatibility: {
          anthropic: manifest.compatibility.claudeCode,
          codex: manifest.compatibility.codex,
        },
      },
    ],
  };
}

async function loadClaudePrompt(skillDir) {
  const sourcePath = path.join(skillDir, 'source', 'claude.md');
  try {
    return await readFile(sourcePath, 'utf8');
  } catch (error) {
    throw new Error(`Missing canonical prompt source at ${sourcePath}: ${error.message}`);
  }
}

async function loadReferenceBundle(skillDir, references) {
  const bundle = [];
  for (const referencePath of references) {
    const absolutePath = path.join(skillDir, referencePath);
    try {
      bundle.push({
        path: referencePath.replaceAll(path.sep, '/'),
        content: (await readFile(absolutePath, 'utf8')).trim(),
      });
    } catch (error) {
      throw new Error(`Missing canonical reference at ${absolutePath}: ${error.message}`);
    }
  }

  return bundle;
}

async function writeTextFile(filePath, content) {
  await mkdir(path.dirname(filePath), { recursive: true });
  await writeFile(filePath, content, 'utf8');
}
