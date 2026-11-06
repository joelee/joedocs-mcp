# Documentation Agent Guide

Documentation is part of the product. Update docs in the same change that makes them true.

## Maintained Documents

- `README.md`: what JoeDocs is and why, features, use cases, install, quick start, and links to deeper docs. It is also the crates.io page, so links are absolute.
- `docs/deployment.md`: installing and running a server (systemd, `brew services`, Docker), remote access, connecting clients, operations and troubleshooting.
- `docs/reference.md`: the stable interfaces users rely on: commands, exit codes, MCP tools and parameters, error codes, search syntax, front matter and history trailers.
- `docs/skills/`: agent skills that use JoeDocs, one folder per skill with a `SKILL.md`, and an index in `docs/skills/README.md`.
- `docs/architecture.md`: system context, major modules, boundaries, key design decisions, and diagrams.
- `docs/configuration.md`: all non-secret config keys, defaults, lookup order, and secret handling rules.
- `docs/directory-structure.md`: repository layout and what belongs in each directory.
- `docs/developer-guide.md`: setup, development workflow, testing, linting, debugging, and releasing.
- `docs/backlog.md`: high-level feature backlog and links to feature directories.

## Rules

- Keep docs concise and specific to this project.
- Prefer examples over abstract guidance when the example removes ambiguity.
- Use Mermaid diagrams for diagrams that should render in Markdown.
- Use PlantUML only when Mermaid cannot express the diagram clearly.
- Do not document secrets or real credentials.
- If behavior, commands, config, or directory structure changes, update the matching document.
- If a feature adds or changes public behavior, update `README.md` or explain why it does not need a README change, and update `docs/reference.md`.
- If a change affects how a server is installed, run or reached, update `docs/deployment.md`.
- If an MCP tool changes, check the skills in `docs/skills/` that call it.
- If a feature changes architecture, update `docs/architecture.md`.
- If a feature changes config, update `docs/configuration.md` and `config.example.toml`, and `.env.example` when secret names change.
- If a feature changes repository layout, update `docs/directory-structure.md`.
- If a feature changes developer workflows, update `docs/developer-guide.md`.
- If a feature is added, dropped, blocked, or completed, update `docs/backlog.md`.
- If release behavior changes, record it in `CHANGELOG.md`.

## Quality Bar

- Documentation must match the current code and commands.
- Avoid future-tense promises unless the item is explicitly in `docs/backlog.md`.
- Link to feature directories for implementation-specific detail.
- Keep root-level documentation approachable for a new developer cloning a generated project.
