// Used by .github/workflows/release.yml before building:
//   node scripts/prepare-release.mjs <beta|stable> <run number>
//   * beta   -> version "<base>-beta.<run number>"  (published as a GitHub prerelease)
//   * stable -> version "<base>"                    (published as a full release)
//   * writes the version into package.json and the release notes (commits since the previous release of the
//     same kind) into build/release-notes.md
//   * prints/exports: version, tag, prerelease (true|false), title
//
// <base> is package.json's version without its prerelease part (0.0.1-beta.1 -> 0.0.1). If a stable release
// v<base> already exists, the base moves up to the next free patch number, so betas made after a stable
// release are always newer than it (0.0.2-beta.N after v0.0.1) and never look like an update downgrade.
// To start a new minor/major line, change "version" in package.json (e.g. to 0.1.0-beta.1).
import fs from "node:fs";
import path from "node:path";
import { execSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const channel = process.argv[2];
const run = process.argv[3];
if (!["beta", "stable"].includes(channel) || (channel === "beta" && !/^\d+$/.test(run || ""))) {
  console.error("Usage: node scripts/prepare-release.mjs <beta|stable> <run number (beta only)>");
  process.exit(1);
}

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const pkgPath = path.join(root, "package.json");
const pkgText = fs.readFileSync(pkgPath, "utf8");
const current = JSON.parse(pkgText).version;
const match = /^(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?$/.exec(current);
if (!match) {
  console.error(`package.json version "${current}" is not x.y.z or x.y.z-prerelease`);
  process.exit(1);
}

const git = (args) => {
  try {
    return execSync(`git ${args}`, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return "";
  }
};

const tags = new Set(git("tag --list").split("\n").filter(Boolean));
let [major, minor, patch] = match.slice(1).map(Number);
while (tags.has(`v${major}.${minor}.${patch}`)) patch += 1;
const base = `${major}.${minor}.${patch}`;

const version = channel === "stable" ? base : `${base}-beta.${run}`;
const tag = `v${version}`;
if (tags.has(tag)) {
  console.error(`Tag ${tag} already exists`);
  process.exit(1);
}

// Only the "version" line is replaced, so the rest of package.json keeps its exact formatting.
fs.writeFileSync(pkgPath, pkgText.replace(/("version"\s*:\s*")[^"]+(")/, `$1${version}$2`));

// Notes: a beta lists the commits since the previous release of any kind; a stable release lists everything
// since the previous stable release (betas included), since stable users skipped all the betas.
const previousTag = channel === "stable" ? git('describe --tags --abbrev=0 --exclude "v*-*"') : git("describe --tags --abbrev=0");
const subjects = git(`log ${previousTag ? `${previousTag}..HEAD` : "HEAD"} --no-merges --format=%s -n ${channel === "stable" ? 60 : 30}`)
  .split("\n")
  .filter(Boolean);
const notes = subjects.length ? subjects.map((s) => `- ${s}`).join("\n") : "- Improvements and fixes";
fs.mkdirSync(path.join(root, "build"), { recursive: true });
fs.writeFileSync(path.join(root, "build", "release-notes.md"), `${notes}\n`);

const prerelease = channel === "beta";
console.log(`Version ${version} (${channel}, tag ${tag}, ${prerelease ? "prerelease" : "full release"})`);
console.log(notes);

if (process.env.GITHUB_OUTPUT) {
  fs.appendFileSync(
    process.env.GITHUB_OUTPUT,
    `version=${version}\ntag=${tag}\nprerelease=${prerelease}\ntitle=Agu Chat ${version}\n`,
  );
}
