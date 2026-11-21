# Rich requests and activity

Conversation Hub v2 keeps the existing request envelope, prompt, choices and question answers. A request can additionally carry a semantic request kind, bounded structured details, options with stable IDs, per-request answerability and observed resolution. Clients use the prompt and choices for an unfamiliar request kind. Details use the keys documented in the item schema and are optional so connectors can report only what their source provides.

A resolution uses one of a legacy label, structured question answers or a semantic option ID with optional feedback or form values. The Hub continues to require the interaction grant and delegates source-specific interpretation and stale-request checks to the connector. Claude's bridged permissions accept the same allow and deny decisions through either spelling. A connector records provenance when observed and leaves a final decision absent when the source cannot establish it.

Activity is published through stable item upserts. An optional parent subagent item ID links each child to its thread, and a file activity can refer to the tool item that produced it. Unknown activity can degrade to the existing unsupported-item presentation. Background work and runtime labels are optional state fields. Attention still follows pending request identity and carries the request kind even when the phone cannot answer.

Request details, activity summaries and answer payloads have explicit size limits. Connector text uses the existing secret and home-path redaction before publication. Interactive URLs use HTTP or HTTPS, and elicitation cards render supported inline JSON Schema controls with a terminal fallback for unsupported forms. Connectors own source behavior and can populate the model incrementally.

| Request kind | Detail fields |
| --- | --- |
| Command | Command, working directory, sandbox and reason |
| File change | Paths, unified diff or per-file previews |
| Network | Host or domain |
| Sandbox escalation and permissions | Filesystem and network scopes, scope label and reason |
| MCP tool | Server, tool and arguments summary |
| App access | App name and access scope |
| Browser | Origin, action and URL |
| Question | Existing structured questions |
| Plan approval | Markdown and plan file path |
| Elicitation form | Message and inline JSON Schema |
| Elicitation URL and sign-in | Message or service, and URL |
| Other | Prompt, choices and any semantic options the connector provides |

Connectors announce a subagent item before emitting nested items that reference its item ID. Mobile renders nested activity in the subagent disclosure and keeps a missing parent's children visible in the transcript. The form card accepts flat objects with scalar fields, defaults, choices and bounds. Unsupported schemas can be answered in the terminal. Opening an HTTP or HTTPS URL requires a tap, and the app reports an advertised `open_url` option only after iOS accepts the handoff.
