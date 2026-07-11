import { spawnSync } from "node:child_process";

// SQLx keeps optional MySQL/Postgres driver packages in Cargo.lock even though
// this workspace enables SQLite only. The optional graph contains rsa, which
// has an unresolved advisory. Do not turn that into a blanket waiver: first
// prove rsa is absent from the target dependency graph, then audit the lock.
const dependencyCheck = spawnSync(
  "cargo",
  ["tree", "--target", "all", "-i", "rsa@0.9.10"],
  { stdio: ["ignore", "pipe", "pipe"], encoding: "utf8" },
);
if (dependencyCheck.status !== 0) {
  process.stderr.write(dependencyCheck.stderr || "cargo tree failed\n");
  process.exit(dependencyCheck.status ?? 1);
}
if (dependencyCheck.stdout.includes("rsa v0.9.10")) {
  console.error("rsa is in the enabled target graph; the audit waiver is no longer valid");
  process.exit(1);
}

const audit = spawnSync(
  "cargo",
  ["audit", "--ignore", "RUSTSEC-2023-0071"],
  { stdio: "inherit" },
);
process.exit(audit.status ?? 1);
