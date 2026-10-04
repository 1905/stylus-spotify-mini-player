import { describe, it, expect } from "vitest";
import { mcpStatusLine, MCP_COPY } from "./mcp.js";

describe("mcpStatusLine", () => {
  it("says what the server does", () => {
    expect(mcpStatusLine(null)).toMatch(/^Off\./);
    expect(mcpStatusLine({ enabled: false, running: false })).toMatch(/^Off\./);
    expect(mcpStatusLine({ enabled: true, running: false, error: "port 5590 is in use" })).toBe("Not running: port 5590 is in use");
    expect(mcpStatusLine({ enabled: true, running: false })).toBe("Starting…");
    expect(mcpStatusLine({ enabled: true, running: true, port: 5590, callsToday: 3 })).toBe("Running on 127.0.0.1:5590 · 3 calls today");
    expect(mcpStatusLine({ enabled: true, running: true, port: 5590, callsToday: 1 })).toBe("Running on 127.0.0.1:5590 · 1 call today");
  });

  it("copies through Rust's texts", () => {
    expect(MCP_COPY.json.slice(0, 2)).toEqual(["mcp_connect_text", { format: "json" }]);
    expect(MCP_COPY.claude.slice(0, 2)).toEqual(["mcp_connect_text", { format: "claude" }]);
    expect(MCP_COPY.skill[0]).toBe("mcp_skill_text");
  });
});
