# Domain docs

This repository uses a single-context domain layout.

## Before exploring

Read `GLOSSARY.md` at the repository root.
Read relevant architectural decision records in `docs/adr/`.

If these files do not exist, proceed silently.
Do not flag their absence or suggest creating them upfront.
The `domain-modeling` skill creates them when terms or decisions are resolved.

## Layout

- `GLOSSARY.md` defines the repository's domain vocabulary.
- `docs/adr/` contains architectural decision records.

## Use the glossary's vocabulary

Use the glossary's terms when naming domain concepts in issues,
proposals, hypotheses, and tests.
Avoid synonyms that the glossary explicitly rejects.

If a needed concept is missing, reconsider whether it belongs to the domain.
Note a genuine vocabulary gap for `domain-modeling`.

## Flag ADR conflicts

If a proposal contradicts an existing ADR, name the ADR and explain
why the decision deserves reconsideration.
Do not silently override it.
