#!/usr/bin/env -S NODE_NO_WARNINGS=1 pnpm ts-node-esm --files

import { Codex } from "@yansigit/nyacode-sdk";
import { codexPathOverride } from "./helpers.ts";
import z from "zod";

const codex = new Codex({ codexPathOverride: codexPathOverride() });
const thread = codex.startThread();

const schema = z.object({
  summary: z.string(),
  status: z.enum(["ok", "action_required"]),
});

const turn = await thread.run("Summarize repository status", {
  outputSchema: z.toJSONSchema(schema, { target: "draft-07" }),
});
console.log(turn.finalResponse);
