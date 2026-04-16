# Claude to Codex Skill Bridge Walkthrough

## Goal

This walkthrough shows the current happy path for the project-local bridge flow:

1. compile and install a canonical skill
2. verify the generated project-local state
3. run the proxy with the generated registry
4. activate the Claude plugin bundle for a local session

## 1. Install the Sample Skill

From the repository root:

```bash
node packages/skills-cli/src/cli.mjs add skills/code-review --agent claude-code --agent codex
```

Expected output includes:

- installed skill id and version
- `.bridge/store/...` location
- a Claude activation hint using `claude --plugin-dir ...`
- a Codex runtime hint using `.bridge/registry.json`

## 2. Inspect the Generated Project State

List installed skills:

```bash
node packages/skills-cli/src/cli.mjs list
```

Validate install health:

```bash
node packages/skills-cli/src/cli.mjs doctor
```

Generated project-local files:

- `.bridge/registry.json`
- `.bridge/install-state.json`
- `.bridge/store/code-review/1.0.0/dist/code-review/SKILL.md`
- `.bridge/store/code-review/1.0.0/dist/code-review/references.json`
- `.bridge/agents/claude-code/code-review`
- `.bridge/agents/codex/code-review`

## 3. Run the Proxy With the Generated Registry

Point the proxy at the aggregated registry:

```bash
cargo run -- --skills-registry-path .bridge/registry.json
```

The proxy should log that it loaded the skill registry and the number of entries.

## 4. Start a Claude Session With the Generated Plugin

Use the generated Claude plugin bundle for a local session:

```bash
claude --plugin-dir .bridge/agents/claude-code/code-review
```

The generated plugin contains:

- `.claude-plugin/plugin.json`
- `commands/code-review.md`

The command file begins with the bridge marker:

```text
skill-bridge:code-review@1.0.0
```

When that marker reaches the proxy through the Anthropic request path, the proxy:

1. strips the marker from the forwarded request
2. resolves the installed skill from `.bridge/registry.json`
3. loads the Codex `SKILL.md`
4. loads `references.json`
5. prepends those instructions before sending the request to Codex

## 5. Update or Remove the Skill

Rebuild installed artifacts:

```bash
node packages/skills-cli/src/cli.mjs update code-review
```

Remove the installed skill:

```bash
node packages/skills-cli/src/cli.mjs remove code-review
```

## Current Limitations

- install source is local-path only
- global install is not implemented
- OpenAI request path skill mapping is not implemented
- tool schema transforms are not implemented
