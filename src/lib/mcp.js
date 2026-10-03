// Settings → MCP server: the status line and the copy actions (Rust mcp.rs makes the texts).

/** The line under the MCP switch, from mcp_status ({enabled, running, port, error, callsToday}). */
export function mcpStatusLine(s) {
  if (!s || !s.enabled) return "Off. Lets AI tools on this Mac (Claude Code, Cursor…) control Needle.";
  if (s.error) return `Not running: ${s.error}`;
  if (!s.running) return "Starting…";
  const n = Number(s.callsToday) || 0;
  return `Running on 127.0.0.1:${s.port} · ${n} ${n === 1 ? "call" : "calls"} today`;
}

/** The copy buttons: data-copy value → [Tauri command, args, what the toast names]. */
export const MCP_COPY = {
  json: ["mcp_connect_text", { format: "json" }, "JSON config"],
  claude: ["mcp_connect_text", { format: "claude" }, "Claude Code command"],
  skill: ["mcp_skill_text", {}, "Skill"],
};
