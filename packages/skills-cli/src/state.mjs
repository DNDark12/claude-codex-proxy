import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';

export const BRIDGE_DIRNAME = '.bridge';
export const STATE_FILENAME = 'install-state.json';
export const REGISTRY_FILENAME = 'registry.json';

export async function loadInstallState(projectRoot) {
  const statePath = getStatePath(projectRoot);

  try {
    const raw = await readFile(statePath, 'utf8');
    const parsed = JSON.parse(raw);
    return {
      version: parsed.version ?? '1',
      scope: parsed.scope ?? 'project',
      skills: Array.isArray(parsed.skills) ? parsed.skills : [],
    };
  } catch (error) {
    if (error.code === 'ENOENT') {
      return defaultInstallState();
    }
    throw error;
  }
}

export async function saveInstallState(projectRoot, state) {
  const statePath = getStatePath(projectRoot);
  await mkdir(path.dirname(statePath), { recursive: true });
  await writeFile(statePath, JSON.stringify(state, null, 2) + '\n', 'utf8');
}

export async function writeAggregatedRegistry(projectRoot, entries) {
  const registryPath = getRegistryPath(projectRoot);
  await mkdir(path.dirname(registryPath), { recursive: true });
  const registry = {
    version: '1',
    skills: [...entries].sort((left, right) =>
      `${left.id}@${left.version}`.localeCompare(`${right.id}@${right.version}`),
    ),
  };
  await writeFile(registryPath, JSON.stringify(registry, null, 2) + '\n', 'utf8');
}

export function getBridgeRoot(projectRoot) {
  return path.join(projectRoot, BRIDGE_DIRNAME);
}

export function getStatePath(projectRoot) {
  return path.join(getBridgeRoot(projectRoot), STATE_FILENAME);
}

export function getRegistryPath(projectRoot) {
  return path.join(getBridgeRoot(projectRoot), REGISTRY_FILENAME);
}

function defaultInstallState() {
  return {
    version: '1',
    scope: 'project',
    skills: [],
  };
}
