// Test helper: the shared sap-core sample, read from disk.
import { readFileSync } from "node:fs";
import type { FlowSnapshot } from "../api/types.ts";

export const fixture = (): FlowSnapshot =>
  JSON.parse(readFileSync(new URL("../../../../crates/sap-core/tests/fixtures/flow.json", import.meta.url), "utf8"));
