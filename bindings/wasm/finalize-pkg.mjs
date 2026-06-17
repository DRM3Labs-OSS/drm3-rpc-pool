// Post-build finalizer for the wasm-pack output.
//
// wasm-pack derives pkg/package.json from the crate name (drm3-rpc-pool-wasm).
// We publish under the scoped name @drm3labs-oss/rpc-pool, so this script:
//   1. rewrites the generated pkg/package.json `name`,
//   2. adds keywords + the .wasm.d.ts to `files`,
//   3. copies README.md into the package dir.
//
// Usage: node finalize-pkg.mjs [pkgDir]   (default: ./pkg)
import { readFileSync, writeFileSync, copyFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkgDir = process.argv[2] ? join(process.cwd(), process.argv[2]) : join(here, "pkg");
const manifestPath = join(pkgDir, "package.json");

if (!existsSync(manifestPath)) {
  console.error(`No package.json at ${manifestPath}. Run wasm-pack build first.`);
  process.exit(1);
}

const pkg = JSON.parse(readFileSync(manifestPath, "utf8"));
pkg.name = "@drm3labs-oss/rpc-pool";
pkg.keywords = ["evm", "rpc", "ethereum", "failover", "wasm", "browser", "drm3"];

const dts = "drm3_rpc_pool_wasm_bg.wasm.d.ts";
if (Array.isArray(pkg.files) && !pkg.files.includes(dts) && existsSync(join(pkgDir, dts))) {
  pkg.files.push(dts);
}
if (Array.isArray(pkg.files) && !pkg.files.includes("README.md")) {
  pkg.files.push("README.md");
}

writeFileSync(manifestPath, JSON.stringify(pkg, null, 2) + "\n");

const readmeSrc = join(here, "README.md");
if (existsSync(readmeSrc)) {
  copyFileSync(readmeSrc, join(pkgDir, "README.md"));
}

console.log(`Finalized ${manifestPath} -> ${pkg.name}`);
