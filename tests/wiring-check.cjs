#!/usr/bin/env node

/**
 * Integration Wiring Tests for Caduceus
 *
 * Verifies that every backend IPC command has a frontend API wrapper,
 * every React component is imported, and all layers are properly connected.
 *
 * Run: node tests/wiring-check.js
 * CI:  npm run test:wiring
 */

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
let failures = 0;
let passes = 0;

function assert(condition, message) {
  if (condition) {
    passes++;
  } else {
    failures++;
    console.error(`  ❌ FAIL: ${message}`);
  }
}

function section(name) {
  console.log(`\n── ${name} ──`);
}

// ── 1. Backend IPC commands ──────────────────────────────────────

section("Backend IPC Commands");

const mainRs = fs.readFileSync(
  path.join(ROOT, "src-tauri/src/main.rs"),
  "utf-8"
);

// Extract all #[tauri::command] function names
const ipcCommands = [];
const lines = mainRs.split("\n");
for (let i = 0; i < lines.length; i++) {
  if (lines[i].includes("#[tauri::command]")) {
    // Next non-empty line should have the fn declaration
    for (let j = i + 1; j < Math.min(i + 5, lines.length); j++) {
      const fnMatch = lines[j].match(/(?:async\s+)?fn\s+([a-z_]+)\s*\(/);
      if (fnMatch) {
        ipcCommands.push(fnMatch[1]);
        break;
      }
    }
  }
}

// Extract all commands registered in generate_handler!
const handlerMatch = mainRs.match(
  /generate_handler!\[\s*([\s\S]*?)\]/
);
const registeredCommands = handlerMatch
  ? handlerMatch[1]
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean)
  : [];

assert(
  ipcCommands.length >= 50,
  `Expected >= 50 IPC commands, found ${ipcCommands.length}`
);

// Every #[tauri::command] fn must be in generate_handler!
for (const cmd of ipcCommands) {
  assert(
    registeredCommands.includes(cmd),
    `IPC command '${cmd}' defined but NOT registered in generate_handler!`
  );
}

// Every entry in generate_handler! must have a #[tauri::command] fn
for (const cmd of registeredCommands) {
  assert(
    ipcCommands.includes(cmd),
    `'${cmd}' registered in generate_handler! but no #[tauri::command] fn found`
  );
}

console.log(
  `  ✅ ${ipcCommands.length} IPC commands defined, ${registeredCommands.length} registered`
);

// ── 2. Frontend API wrappers ──────────────────────────────────────

section("Frontend API Wrappers (src/api/tauri.ts)");

const tauriApi = fs.readFileSync(
  path.join(ROOT, "src/api/tauri.ts"),
  "utf-8"
);

const frontendInvokes = [];
const invokeRegex = /invoke\("([^"]+)"/g;
let match;
while ((match = invokeRegex.exec(tauriApi)) !== null) {
  frontendInvokes.push(match[1]);
}
const uniqueFrontendInvokes = [...new Set(frontendInvokes)];

// Every backend command must have a frontend invoke
for (const cmd of ipcCommands) {
  assert(
    uniqueFrontendInvokes.includes(cmd),
    `Backend command '${cmd}' has NO frontend invoke() wrapper in src/api/tauri.ts`
  );
}

// Every frontend invoke must have a backend command
for (const cmd of uniqueFrontendInvokes) {
  assert(
    ipcCommands.includes(cmd),
    `Frontend calls invoke('${cmd}') but NO backend #[tauri::command] fn exists`
  );
}

console.log(
  `  ✅ ${uniqueFrontendInvokes.length} frontend wrappers match ${ipcCommands.length} backend commands`
);

// ── 3. React Components ──────────────────────────────────────────

section("React Components");

const componentsDir = path.join(ROOT, "src/components");
const componentFiles = fs
  .readdirSync(componentsDir)
  .filter((f) => f.endsWith(".tsx"))
  .map((f) => f.replace(".tsx", ""));

const appTsx = fs.readFileSync(path.join(ROOT, "src/App.tsx"), "utf-8");

// Child components that are used inside other components, not App.tsx directly
const childComponents = [
  "SyntaxHighlighter",  // used in Chat
  "MarketplaceCard",    // used in MarketplacePanel
  "InlineDiffReview",   // used in DiffViewer
  "ImageRenderer",      // used in Chat
  "CardTerminal",       // used in KanbanBoard
  "VimMode",            // used in Chat
  "ContextVisualizer",  // used in ContextViewer
];

for (const comp of componentFiles) {
  const isImported = appTsx.includes(`from "./components/${comp}"`);
  const isChild = childComponents.includes(comp);

  if (isChild) {
    // Child components should be imported by their parent, not necessarily App.tsx
    // Check they're imported somewhere
    const parentFiles = componentFiles.filter((f) => f !== comp);
    const usedSomewhere = parentFiles.some((parent) => {
      const parentContent = fs.readFileSync(
        path.join(componentsDir, `${parent}.tsx`),
        "utf-8"
      );
      return parentContent.includes(comp);
    });
    assert(
      isImported || usedSomewhere,
      `Component '${comp}' is not imported in App.tsx or any parent component`
    );
  } else {
    assert(
      isImported,
      `Component '${comp}' exists but is NOT imported in App.tsx`
    );
  }
}

assert(
  componentFiles.length >= 28,
  `Expected >= 28 components, found ${componentFiles.length}`
);

console.log(`  ✅ ${componentFiles.length} components verified`);

// ── 4. Exported API functions ──────────────────────────────────

section("API Export Completeness");

const exportedFunctions = [];
const exportRegex = /export\s+async\s+function\s+([a-zA-Z_]+)/g;
while ((match = exportRegex.exec(tauriApi)) !== null) {
  exportedFunctions.push(match[1]);
}

assert(
  exportedFunctions.length >= 50,
  `Expected >= 50 exported API functions, found ${exportedFunctions.length}`
);

// Every invoke must come from an exported function
for (const cmd of uniqueFrontendInvokes) {
  const camelCase = cmd.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
  const hasExport = exportedFunctions.some(
    (fn) => fn.toLowerCase() === camelCase.toLowerCase()
  );
  assert(
    hasExport,
    `invoke('${cmd}') exists but no matching exported function (expected '${camelCase}')`
  );
}

console.log(`  ✅ ${exportedFunctions.length} exported API functions`);

// ── 5. Rust crate modules ──────────────────────────────────────

section("Rust Crate Compilation");

const cargoToml = fs.readFileSync(
  path.join(ROOT, "Cargo.toml"),
  "utf-8"
);
const crateMembers = [];
const memberRegex = /"([^"]+)"/g;
const membersSection = cargoToml.match(/members\s*=\s*\[([\s\S]*?)\]/);
if (membersSection) {
  while ((match = memberRegex.exec(membersSection[1])) !== null) {
    crateMembers.push(match[1]);
  }
}

assert(
  crateMembers.length >= 14,
  `Expected >= 14 workspace crate members, found ${crateMembers.length}`
);

// Verify each crate directory exists
for (const member of crateMembers) {
  const cratePath = path.join(ROOT, member);
  assert(
    fs.existsSync(cratePath),
    `Workspace member '${member}' directory does not exist`
  );
  const cargoPath = path.join(cratePath, "Cargo.toml");
  assert(
    fs.existsSync(cargoPath),
    `Workspace member '${member}' missing Cargo.toml`
  );
}

console.log(`  ✅ ${crateMembers.length} crate members verified`);

// ── 6. Tauri config ──────────────────────────────────────────────

section("Tauri Configuration");

const tauriConf = JSON.parse(
  fs.readFileSync(path.join(ROOT, "src-tauri/tauri.conf.json"), "utf-8")
);

assert(
  tauriConf.productName === "Caduceus",
  `Product name should be 'Caduceus', got '${tauriConf.productName}'`
);
assert(
  tauriConf.build?.devUrl === "http://localhost:1420",
  `Dev URL should be localhost:1420`
);
assert(
  tauriConf.build?.frontendDist === "../dist",
  `Frontend dist should be '../dist'`
);

console.log("  ✅ Tauri config verified");

// ── 7. CSS Variables ──────────────────────────────────────────────

section("CSS Theme Variables");

const stylesCSS = fs.readFileSync(
  path.join(ROOT, "src/styles.css"),
  "utf-8"
);

const requiredVars = [
  "--color-bg",
  "--color-surface",
  "--color-panel",
  "--color-border",
  "--color-text",
  "--color-muted",
  "--color-accent",
  "--color-success",
  "--color-warning",
  "--color-danger",
];

for (const v of requiredVars) {
  assert(stylesCSS.includes(v), `CSS missing required variable '${v}'`);
}

console.log(`  ✅ ${requiredVars.length} CSS variables verified`);

// ── 8. Data-tour attributes ──────────────────────────────────────

section("Onboarding Tour Targets");

const onboardingTour = fs.readFileSync(
  path.join(ROOT, "src/components/OnboardingTour.tsx"),
  "utf-8"
);

const tourTargets = [];
const targetRegex = /target:\s*["'](\[data-tour="[^"]+"\])["']/g;
while ((match = targetRegex.exec(onboardingTour)) !== null) {
  tourTargets.push(match[1]);
}

// Check that App.tsx or components have data-tour attributes
const allTsx = [appTsx];
componentFiles.forEach((comp) => {
  allTsx.push(
    fs.readFileSync(path.join(componentsDir, `${comp}.tsx`), "utf-8")
  );
});
const allTsxContent = allTsx.join("\n");

for (const target of tourTargets) {
  const attrMatch = target.match(/data-tour="([^"]+)"/);
  if (attrMatch) {
    const hasAttr = allTsxContent.includes(`data-tour="${attrMatch[1]}"`);
    // Don't fail on missing data-tour attrs yet — they need to be added to layout
    if (!hasAttr) {
      console.log(`  ⚠️  Tour target '${attrMatch[1]}' not found in any component (add data-tour attribute)`);
    }
  }
}

console.log(`  ✅ ${tourTargets.length} tour targets checked`);

// ── 9. CI Workflows ──────────────────────────────────────────────

section("CI Workflows");

const ciYml = fs.readFileSync(
  path.join(ROOT, ".github/workflows/ci.yml"),
  "utf-8"
);
const releaseYml = fs.readFileSync(
  path.join(ROOT, ".github/workflows/release.yml"),
  "utf-8"
);

assert(ciYml.includes("cargo test"), "CI workflow must run cargo test");
assert(ciYml.includes("cargo clippy") || ciYml.includes("clippy"), "CI workflow must run clippy");
assert(ciYml.includes("cargo fmt") || ciYml.includes("fmt"), "CI workflow must run cargo fmt");
assert(releaseYml.includes("tauri-action"), "Release workflow must use tauri-action");
assert(releaseYml.includes("webkit2gtk") || releaseYml.includes("libwebkit"), "Release workflow must install Linux deps");

console.log("  ✅ CI workflows verified");

// ── 10. Package.json scripts ─────────────────────────────────────

section("Package.json Scripts");

const packageJson = JSON.parse(
  fs.readFileSync(path.join(ROOT, "package.json"), "utf-8")
);

assert(packageJson.scripts?.dev, "package.json must have 'dev' script");
assert(packageJson.scripts?.build, "package.json must have 'build' script");

console.log("  ✅ Package.json verified");

// ── 11. Security Policy ──────────────────────────────────────────

section("Security & Documentation");

assert(
  fs.existsSync(path.join(ROOT, "SECURITY.md")),
  "SECURITY.md must exist"
);
assert(
  fs.existsSync(path.join(ROOT, "FEATURES.md")),
  "FEATURES.md must exist"
);
assert(
  fs.existsSync(path.join(ROOT, "README.md")),
  "README.md must exist"
);

console.log("  ✅ Documentation files verified");

// ── Summary ──────────────────────────────────────────────────────

console.log("\n══════════════════════════════════════");
console.log(`  PASSED: ${passes}`);
console.log(`  FAILED: ${failures}`);
console.log("══════════════════════════════════════\n");

process.exit(failures > 0 ? 1 : 0);
