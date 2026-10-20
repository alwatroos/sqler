# ADR-0000: ADR process

- Status: accepted
- Date: 2026-10-20
- Deciders: SQLer maintainers

## Context and Problem Statement

SQLer makes decisions that are expensive to reverse: the UI framework, the plugin ABI, the AI tool
contract and storage formats. Later contributors need a place to find why a decision was made, and
a rule for when one must be written.

## Decision Drivers

- Decisions must be findable and citable from code reviews and the book.
- The format must be light enough that people actually write ADRs.
- The format must render correctly in mdBook.

## Considered Options

- MADR (Markdown Architectural Decision Records), with metadata as a bullet list.
- Nygard short form.
- No ADRs.

## Decision Outcome

Chosen option: MADR, with metadata as a bullet list instead of YAML front matter, because mdBook
does not strip front matter and would render it as a heading.

Required layout of every ADR:

```text
# ADR-NNNN: <Title>

- Status: proposed | accepted | rejected | deprecated | superseded by ADR-XXXX
- Date: YYYY-MM-DD
- Deciders: SQLer maintainers

## Context and Problem Statement
## Decision Drivers
## Considered Options
## Decision Outcome
### Consequences
### Confirmation
## Pros and Cons of the Options
## More Information
```

Rules:

- When an ADR is required: before a new dependency, crate, public API or cross-cutting change, and
  before decisions on the framework, the plugin ABI, the AI tool contract and the storage format.
- Numbering: the next free four-digit number, in files named `NNNN-title.md` under `docs/src/adr/`.
- Lifecycle: `proposed`, then `accepted`.
- Superseding: add a new ADR, set the old ADR's Status to `superseded by ADR-XXXX`, and change
  nothing else in the old ADR.
- ADRs are never deleted.
- Each new ADR adds a line to `SUMMARY.md`, a row to the table in [the ADR index](index.md) and a
  row to the ADR table in `AGENTS.md` section 8.

### Consequences

- Good: decisions are citable and survive contributor turnover.
- Bad: a small writing cost before some changes.

### Confirmation

Reviewers check that a PR introducing a dependency, crate, public API or cross-cutting change links
its ADR, and that the three index locations above are updated.

## Pros and Cons of the Options

### MADR

- Good: widely used, structured sections, explicit options and consequences.
- Bad: more sections than the short form.

### Nygard short form

- Good: minimal.
- Bad: no explicit options or drivers, so rejected alternatives are not recorded.

### No ADRs

- Good: no overhead.
- Bad: decisions are lost in chat and PR threads.

## More Information

- [MADR](https://adr.github.io/madr/)
