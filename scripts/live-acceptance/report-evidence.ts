// Shared CI failure reporter.
//
// The harnesses already print bounded failure lines from the same formatter at
// exit. This entry point exists for the case where the runner still sees a
// nonzero status after that point (for example the process was killed before
// the final write, or a harness did not reach its shutdown path): it reuses the
// one implementation instead of keeping a second parser in the shell script.
import { readFileSync } from "node:fs";
import { failureLines, type Evidence } from "./jcode-live-common.ts";

const [file, label] = process.argv.slice(2);
if (!file || !label) {
  console.error("usage: report-evidence.ts <evidence.json> <label>");
  process.exit(2);
}

let evidence: Evidence;
try {
  evidence = JSON.parse(readFileSync(file, "utf8")) as Evidence;
} catch (error) {
  console.error(
    `FAILED ${label}: harness exited without readable evidence (${file}): ${error instanceof Error ? error.message : String(error)}`,
  );
  process.exit(0);
}

const failed = Object.values(evidence.scenarios ?? {}).filter(
  (scenario) => scenario.status === "fail",
).length;
if (failed === 0) {
  console.error(`${label}: no failed scenario recorded, but the harness exited nonzero`);
}
for (const line of failureLines(evidence)) console.error(line);
