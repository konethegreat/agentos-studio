# Text statistics (MCP bundle)

An official AgentOS Studio MCP bundle, signed with `k-one-official-1`: a stdio server named
`text-stats` with two read-only tools, `count_words` and `count_lines`.

Registering it from Capabilities writes `mcp.text-stats.count_words = ask`,
`mcp.text-stats.count_lines = ask` and `mcp.text-stats.* = ask` into the tool policy (a tool your
policy already denies stays `deny`), and registers the server as
`agentos-studio --extension-mcp solutions.k-one.text-stats` for the agents you pick. That command
re-verifies the pack against the hash approved at registration and starts the server with an
empty environment in a sandbox folder.

K-ONE IT SOLUTIONS, Kone Tshivhinda.
