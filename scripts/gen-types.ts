// Regenerates packages/api-types/src/generated from Rust types (ADR 0009).
//   node scripts/gen-types.ts          write the bindings
//   node scripts/gen-types.ts --check  also fail if they differ from git
import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const GENERATED_DIR = "packages/api-types/src/generated";
const checkOnly = process.argv.includes("--check");

// Export into an empty scratch folder, so a renamed or deleted Rust type leaves
// no stale file behind and a failed build leaves the committed files untouched.
const scratch = mkdtempSync(path.join(tmpdir(), "callsheet-types-"));
try {
  execFileSync(
    "cargo",
    ["test", "--quiet", "-p", "cs-core", "export_bindings"],
    { stdio: "inherit", env: { ...process.env, TS_RS_EXPORT_DIR: scratch } },
  );
  rmSync(GENERATED_DIR, { recursive: true, force: true });
  cpSync(scratch, GENERATED_DIR, { recursive: true });
} finally {
  rmSync(scratch, { recursive: true, force: true });
}

if (checkOnly) {
  // Porcelain lines are "XY path": Y is the working tree against the index, so
  // anything but a space there (or an untracked "??") is a regenerated diff.
  const changes = execFileSync(
    "git",
    ["status", "--porcelain", "--untracked-files=all", "--", GENERATED_DIR],
    { encoding: "utf8" },
  )
    .split("\n")
    .filter((line) => line.length > 1 && line[1] !== " ")
    .join("\n");
  if (changes) {
    console.error(
      `Generated types differ from the committed ones. Run \`pnpm gen-types\` and commit:\n${changes}`,
    );
    process.exit(1);
  }
}
