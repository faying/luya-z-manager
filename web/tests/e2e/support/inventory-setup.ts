// Clears last run's inventory results so the teardown summary only covers this run.
import { rmSync } from "node:fs";
import { resolve } from "node:path";

export default function globalSetup() {
  rmSync(resolve(__dirname, "../../../test-results/inventory"), { recursive: true, force: true });
}
