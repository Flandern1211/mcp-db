#!/usr/bin/env bun
import { runCli } from "./cli/index.ts";

try {
  process.exitCode = await runCli(process.argv.slice(2));
} catch (err) {
  console.error(err instanceof Error ? err.message : String(err));
  process.exitCode = 1;
}