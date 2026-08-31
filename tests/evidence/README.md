# Contract evidence format

Phase 0 evidence uses the versioned [`v1.schema.json`](v1.schema.json) format. The schema is intentionally closed: every property and value class is allow-listed, and there is no field for provider records, credentials, request payloads, free-form messages, local paths, or machine details.

An evidence record contains only:

- the schema version, contract layer, gate, and pass/fail/skip result;
- the tested source revision;
- the synthetic fixture name;
- the four pinned contract versions; and
- bounded check names with their outcomes.

Failure records use one of the schema's fixed reason codes. They do not carry diagnostic text. Raw provider responses and local execution evidence stay outside the repository and must be redacted before any result is copied into a public record.

The committed schema is the contract. Later layers may add an allow-listed gate or reason code only in a new schema version and a separately reviewed change.
