# AI

SQLer's AI features are planned (M5). The tool contract is recorded in
[ADR-0003](../adr/0003-ai-tool-contract.md).

Providers, per-use-case models, redaction and limits are configured in `ai.toml`, see
[AI configuration](../user-guide/ai.md).

Planned topics:

- [Providers](providers.md): the provider contract (ADR-0034)
- [Tool contract](tool-contract.md): tools, guards and host interfaces (ADR-0036)
- [Safety](safety.md): redaction, untrusted-data envelope and audit log (ADR-0037)
- [Prompts and assist](prompts.md): NL to SQL, explain, fix, `search_data`, prompt templates and grounding (ADR-0038)
- [MCP server](mcp.md): `sqler-cli mcp` over stdio or loopback HTTP (ADR-0040)
