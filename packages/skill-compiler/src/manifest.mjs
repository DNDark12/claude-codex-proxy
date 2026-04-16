import { readFile } from 'node:fs/promises';
import path from 'node:path';

const ALLOWED_MERGE_MODES = new Set(['prepend', 'append', 'replace']);

export async function loadSkillManifest(skillDir) {
  const manifestPath = path.join(skillDir, 'skill.yaml');
  const raw = await readFile(manifestPath, 'utf8');
  const parsed = parseJsonCompatibleYaml(raw, manifestPath);
  return validateSkillManifest(parsed);
}

export function parseJsonCompatibleYaml(raw, manifestPath = 'skill.yaml') {
  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new Error(
      `Invalid manifest at ${manifestPath}: MVP compiler expects JSON-compatible YAML (${error.message})`,
    );
  }
}

export function validateSkillManifest(input) {
  if (!isRecord(input)) {
    throw new Error('Invalid manifest: root must be an object');
  }

  const manifest = {
    schemaVersion: readString(input, 'schema_version'),
    id: readString(input, 'id'),
    version: readString(input, 'version'),
    displayName: readString(input, 'display_name'),
    description: readString(input, 'description'),
    activation: {
      marker: readString(readRecord(input, 'activation'), 'marker', 'activation'),
    },
    compatibility: {
      claudeCode: readBoolean(
        readRecord(input, 'compatibility'),
        'claude_code',
        'compatibility',
      ),
      codex: readBoolean(readRecord(input, 'compatibility'), 'codex', 'compatibility'),
    },
    mapping: {
      codexSkill: readString(readRecord(input, 'mapping'), 'codex_skill', 'mapping'),
      mergeMode: readOptionalString(readRecord(input, 'mapping'), 'merge_mode', 'mapping') ?? 'prepend',
    },
    artifacts: {
      codexEntry: readString(readRecord(input, 'artifacts'), 'codex_entry', 'artifacts'),
      claudeEntry: readString(readRecord(input, 'artifacts'), 'claude_entry', 'artifacts'),
    },
    references: readStringArray(input.references, 'references'),
    toolAliases: readStringMap(input.tool_aliases, 'tool_aliases'),
  };

  if (manifest.schemaVersion !== '1') {
    throw new Error(`Invalid manifest.schema_version: expected "1", received "${manifest.schemaVersion}"`);
  }

  if (!ALLOWED_MERGE_MODES.has(manifest.mapping.mergeMode)) {
    throw new Error(
      `Invalid manifest.mapping.merge_mode: expected one of prepend|append|replace, received "${manifest.mapping.mergeMode}"`,
    );
  }

  return manifest;
}

function readRecord(input, key, parentPath = 'manifest') {
  const value = input[key];
  const pathLabel = `${parentPath}.${key}`;
  if (!isRecord(value)) {
    throw new Error(`Invalid ${pathLabel}: expected object`);
  }
  return value;
}

function readString(input, key, parentPath = 'manifest') {
  const value = input[key];
  const pathLabel = `${parentPath}.${key}`;
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`Invalid ${pathLabel}: expected non-empty string`);
  }
  return value.trim();
}

function readOptionalString(input, key, parentPath = 'manifest') {
  const value = input[key];
  const pathLabel = `${parentPath}.${key}`;
  if (value == null) {
    return null;
  }
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`Invalid ${pathLabel}: expected non-empty string`);
  }
  return value.trim();
}

function readBoolean(input, key, parentPath = 'manifest') {
  const value = input[key];
  const pathLabel = `${parentPath}.${key}`;
  if (typeof value !== 'boolean') {
    throw new Error(`Invalid ${pathLabel}: expected boolean`);
  }
  return value;
}

function readStringMap(value, pathLabel) {
  if (value == null) {
    return {};
  }
  if (!isRecord(value)) {
    throw new Error(`Invalid ${pathLabel}: expected object`);
  }

  return Object.fromEntries(
    Object.entries(value).map(([key, rawValue]) => {
      if (typeof rawValue !== 'string' || rawValue.trim() === '') {
        throw new Error(`Invalid ${pathLabel}.${key}: expected non-empty string`);
      }
      return [key, rawValue.trim()];
    }),
  );
}

function readStringArray(value, pathLabel) {
  if (value == null) {
    return [];
  }
  if (!Array.isArray(value)) {
    throw new Error(`Invalid ${pathLabel}: expected array`);
  }
  return value.map((entry, index) => {
    if (typeof entry !== 'string' || entry.trim() === '') {
      throw new Error(`Invalid ${pathLabel}[${index}]: expected non-empty string`);
    }
    return entry.trim();
  });
}

function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
