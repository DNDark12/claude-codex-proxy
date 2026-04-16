import path from 'node:path';

import { addSkill, doctorProject, listInstalledSkills, removeSkills, updateSkills } from './install.mjs';

async function main() {
  const [command, ...rest] = process.argv.slice(2);

  if (command === 'add') {
    const args = parseAddArgs(rest);
    const result = await addSkill({
      source: args.source,
      projectRoot: process.cwd(),
      agents: args.agents,
      installMode: args.installMode,
    });
    process.stdout.write(
      [
        `Installed ${result.id}@${result.version}`,
        `Source: ${result.source}`,
        `Agents: ${result.agents.join(', ')}`,
        `Mode: ${result.installMode}`,
        `Store: ${result.storeDir}`,
        ...(result.activationHints.claudeCode
          ? [`Claude activation: ${result.activationHints.claudeCode}`]
          : []),
        ...(result.activationHints.codex
          ? [`Codex runtime hint: ${result.activationHints.codex}`]
          : []),
      ].join('\n') + '\n',
    );
    return;
  }

  if (command === 'list') {
    const rows = await listInstalledSkills(process.cwd());
    if (rows.length === 0) {
      process.stdout.write('No installed skills\n');
      return;
    }

    for (const row of rows) {
      process.stdout.write(
        `${row.id}@${row.version} agents=${row.agents.join(',')} mode=${row.installMode} source=${row.source}\n`,
      );
      if (row.activationHints?.claudeCode) {
        process.stdout.write(`  claude=${row.activationHints.claudeCode}\n`);
      }
      if (row.activationHints?.codex) {
        process.stdout.write(`  codex=${row.activationHints.codex}\n`);
      }
    }
    return;
  }

  if (command === 'remove') {
    if (rest.length === 0) {
      throw new Error('Usage: node src/cli.mjs remove <skill-id> [skill-id...]');
    }
    const removed = await removeSkills({
      projectRoot: process.cwd(),
      skillIds: rest,
    });
    process.stdout.write(`Removed ${removed.length} skill(s)\n`);
    return;
  }

  if (command === 'update') {
    const updated = await updateSkills({
      projectRoot: process.cwd(),
      skillIds: rest,
    });
    process.stdout.write(`Updated ${updated.length} skill(s)\n`);
    return;
  }

  if (command === 'doctor') {
    const report = await doctorProject(process.cwd());
    if (report.healthy) {
      process.stdout.write(`OK installed_skills=${report.installedSkills}\n`);
      return;
    }

    for (const issue of report.issues) {
      process.stdout.write(`${issue.code}: ${issue.message}\n`);
    }
    process.exitCode = 1;
    return;
  }

  throw new Error(
    'Usage:\n  node src/cli.mjs add <skill-source> [--agent <name>] [--copy]\n  node src/cli.mjs list\n  node src/cli.mjs remove <skill-id> [skill-id...]\n  node src/cli.mjs update [skill-id...]\n  node src/cli.mjs doctor\n',
  );
}

function parseAddArgs(args) {
  const source = args[0];
  if (!source) {
    throw new Error('Usage: node src/cli.mjs add <skill-source> [--agent <name>] [--copy]');
  }

  const agents = [];
  for (let index = 1; index < args.length; index += 1) {
    const value = args[index];
    if (value === '--agent') {
      const agent = args[index + 1];
      if (!agent) {
        throw new Error('Missing value for --agent');
      }
      agents.push(agent);
      index += 1;
      continue;
    }

    if (value === '--copy') {
      continue;
    }

    throw new Error(`Unknown argument "${value}"`);
  }

  return {
    source: path.resolve(process.cwd(), source),
    agents,
    installMode: args.includes('--copy') ? 'copy' : 'symlink',
  };
}

main().catch((error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
