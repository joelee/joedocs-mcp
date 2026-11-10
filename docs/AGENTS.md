# Documentation Agent Guide

Documentation is part of the product. Update docs in the same change that makes them true.

## Maintained Documents

- `README.md`: what JoeDocs is and why, features, use cases, install, quick start, and links to deeper docs. It is also the crates.io page, so links are absolute.
- `docs/upgrading.md`: upgrading a server: backing up, each install method, checking, going back, and notes for each version.
- `docs/deployment.md`: installing and running a server (systemd, `brew services`, Docker), remote access, connecting clients, operations and troubleshooting.
- `docs/reference.md`: the stable interfaces users rely on: commands, exit codes, MCP tools and parameters, error codes, search syntax, front matter and history trailers.
- `docs/skills/`: agent skills that use JoeDocs, one folder per skill with a `SKILL.md`, and an index in `docs/skills/README.md`.
- `docs/architecture.md`: system context, major modules, boundaries, key design decisions, and diagrams.
- `docs/configuration.md`: all non-secret config keys, defaults, lookup order, and secret handling rules.
- `docs/directory-structure.md`: repository layout and what belongs in each directory.
- `docs/developer-guide.md`: setup, development workflow, testing, linting, debugging, and releasing.
- The backlog is not a file here: it is JoeDocs document JD-22 ("Backlog", a multi-file `notes` document with one `items/<slug>` file per item and `state` and `kind` in each item's front matter), and the `README.md` Roadmap section is its public summary. Bugs and incidents are `issues` documents (`docs/skills/joedocs-issue-log`).

## Rules

- Keep docs concise and specific to this project.
- Prefer examples over abstract guidance when the example removes ambiguity.
- Use Mermaid diagrams for diagrams that should render in Markdown.
- Use PlantUML only when Mermaid cannot express the diagram clearly.
- Do not document secrets or real credentials.
- If behavior, commands, config, or directory structure changes, update the matching document.
- If a feature adds or changes public behavior, update `README.md` or explain why it does not need a README change, and update `docs/reference.md`.
- If a change affects how a server is installed, run or reached, update `docs/deployment.md`.
- If a release adds a migration or changes what an upgrade needs, add a version note to `docs/upgrading.md`.
- If an MCP tool changes, check the skills in `docs/skills/` that call it.
- If a feature changes architecture, update `docs/architecture.md`.
- If a feature changes config, update `docs/configuration.md` and `config.example.toml`, and `.env.example` when secret names change.
- If a feature changes repository layout, update `docs/directory-structure.md`.
- If a feature changes developer workflows, update `docs/developer-guide.md`.
- If a feature is added, dropped, blocked, or completed, update its item in the JoeDocs Backlog (JD-22): add it, change its `state`, or archive it (`Shipped in X.Y.Z`, or why it was dropped). Change the `README.md` Roadmap in the same change when a planned or future headline appears or goes.
- If release behavior changes, record it in `CHANGELOG.md`.

## Quality Bar

- Documentation must match the current code and commands.
- Avoid future-tense promises unless the item is explicitly in the JoeDocs Backlog (JD-22) and the `README.md` Roadmap.
- Link to feature directories for implementation-specific detail.
- Keep root-level documentation approachable for a new developer cloning a generated project.
