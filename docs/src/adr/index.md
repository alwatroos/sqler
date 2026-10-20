# Architecture decisions

An Architecture Decision Record (ADR) captures one significant decision, its context and its
consequences. SQLer uses the MADR format.

- ADRs are never deleted. A changed decision is recorded by a new ADR that supersedes the old one.
- Files are named `NNNN-title.md`, with the next free four-digit number.
- The full process is in [ADR-0000](0000-adr-process.md).

| ADR | Title | Status |
| --- | ----- | ------ |
| [0000](0000-adr-process.md) | ADR process | accepted |
| [0001](0001-ui-framework-spike-plan.md) | UI framework spike plan | proposed |
| [0002](0002-plugin-abi-wasm-component-model.md) | Plugin ABI: WASM Component Model | accepted |
| [0003](0003-ai-tool-contract.md) | AI tool contract | accepted |
| [0004](0004-driver-api.md) | Driver API: boxed futures, event stream, session cancellation | accepted |
| [0005](0005-config-files-and-keybindings.md) | Config files and keybindings: TOML files, chord grammar, std-only directory lookup | accepted |
