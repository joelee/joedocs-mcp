# Documentation Agent Guide

Documentation is part of the product. Update docs in the same change that makes them true.

## Maintained Documents

- `README.md`: project overview, quick start, common commands, and links to deeper docs.
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
- If a feature adds or changes public behavior, update `README.md` or explain why it does not need a README change.
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
