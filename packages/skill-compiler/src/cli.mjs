import path from 'node:path';

import { compileSkill } from './compiler.mjs';
import { loadSkillManifest } from './manifest.mjs';

async function main() {
  const [command, ...rest] = process.argv.slice(2);

  if (command === 'compile') {
    const { skillDir, outDir } = parseCompileArgs(rest);
    const result = await compileSkill({
      skillDir: path.resolve(skillDir),
      outDir: path.resolve(outDir),
    });
    process.stdout.write(
      [
        `Compiled skill ${result.manifest.id}@${result.manifest.version}`,
        `Codex artifact: ${result.codexSkillPath}`,
        `Claude plugin: ${result.claudePluginPath}`,
        `Registry: ${result.registryPath}`,
      ].join('\n') + '\n',
    );
    return;
  }

  if (command === 'validate') {
    const skillDir = rest[0];
    if (!skillDir) {
      throw new Error('Usage: node src/cli.mjs validate <skill-dir>');
    }
    const manifest = await loadSkillManifest(path.resolve(skillDir));
    process.stdout.write(`Validated skill ${manifest.id}@${manifest.version}\n`);
    return;
  }

  throw new Error(
    'Usage:\n  node src/cli.mjs compile <skill-dir> --out <out-dir>\n  node src/cli.mjs validate <skill-dir>\n',
  );
}

function parseCompileArgs(args) {
  const skillDir = args[0];
  if (!skillDir) {
    throw new Error('Usage: node src/cli.mjs compile <skill-dir> --out <out-dir>');
  }

  const outIndex = args.indexOf('--out');
  if (outIndex === -1 || !args[outIndex + 1]) {
    throw new Error('Missing required --out <out-dir> argument');
  }

  return {
    skillDir,
    outDir: args[outIndex + 1],
  };
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
